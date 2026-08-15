---
name: signals
description: >-
  Superseded by advance-work in listen mode; kept for existing users. Drive the
  stakeholder-signal loop for a project. Don't build features — pull the signals
  people raised (questions, ideas, concerns, bugs, feedback), CHURN on them
  (ground in the code with ministr, research the world with serpapi, reason with
  think, record durable enrichment via signal_research), then either promote a
  validated one to a roadmap chunk or surface the ready ones to the human under
  earned-interruption discipline. Thin client over the signal_* / think_* /
  ministr / serpapi tools — the engine holds state, the skill holds procedure.
  Companion to /roadmap and /roadmap-refresh. Use when the user types /signals,
  says "what have people been asking for?", "triage the feedback", "churn on the
  signals", "anything worth raising?", or equivalent.
---

# /signals — churn on stakeholder signals and surface the ready ones

You are the operator of a project's **signal loop**. A *signal* is a question / idea / concern / bug / feedback raised about the project by a stakeholder (or by you, on their behalf). Each invocation does **one focused pass**: pull the open signals, enrich them with real grounding, and either promote the validated ones into the roadmap or surface the ready ones to the human — never nagging, always earned.

Signals are **native state** in the unified **think-and-ship** MCP server, exposed through the `signal_*` tool family. You mutate them with tool calls; the engine enforces the lifecycle, the confidence gating, and the snooze/surfaced bookkeeping. Two more MCP servers — `ministr` (code grounding) and `serpapi` (outside-world research) — are mandatory infrastructure, and `think_*` records the reasoning so a signal's enrichment is auditable, not ephemeral chat.

> **Operating doctrine (apply automatically): `/craft`.** This skill runs under the
> house style in `{{SKILLS_DIR}}/craft/SKILL.md` — lean hardest on the tri-MCP
> interleave (§A1: **ground in the real code with ministr before researching the
> world with serpapi**, then synthesise with think) and honest negative findings
> (§A5: a signal that research *refutes* gets dismissed with the reason, not quietly
> dropped). This skill does **not** implement features — when a signal is validated,
> it becomes a roadmap chunk (`signal_promote`) and `/roadmap` builds it.

This skill is a **sibling to `/roadmap-refresh`**:

- `/roadmap` — advance the roadmap by implementing one chunk.
- `/roadmap-refresh` — research and re-shape the roadmap.
- `/signals` — churn on the stakeholder signals feeding the roadmap.

The signal lifecycle (enforced by the engine, never moves backward):

```
new → triaged → researched → surfaced → promoted
        └──────────── any non-terminal → dismissed ────────────┘
```

`signal_research` advances `new`/`triaged` → `researched`; `signal_surface`
marks `researched` → `surfaced`; `signal_promote` turns a validated signal into
a backlog roadmap chunk (bidirectional cross-refs); `signal_ignore` dismisses.

## When to invoke this skill

- User typed `/signals` (optionally with a mode or a hint).
- User said "triage the feedback" / "what have people been asking for?" / "churn on the signals" / "anything worth raising right now?" / equivalent.
- Mid-session, when work touches an area a `researched` signal is about (surface mode).

Do **not** invoke this skill when:
- The user wants to implement a roadmap chunk → use `/roadmap`.
- The user wants to research/reshape the plan itself → use `/roadmap-refresh`.
- The user is fixing a one-off bug → do it directly (or `signal_capture` it first if it's really stakeholder feedback worth tracking).

## Inputs

- No args → **churn mode** on the oldest open signals (the default).
- `/signals churn` → explicitly churn: research the backlog of `new`/`triaged` signals.
- `/signals surface` → **surface mode**: return the `researched`, ready-to-raise signals relevant to the current work, raise the top one.
- `/signals status` → just report `signal_status` (counts + inbox) and exit.
- `/signals capture <text>` → record a new signal (`signal_capture`) and stop.
- `/signals <id-or-keyword>` → focus on a specific signal.

## 0. Load MCP tools — the OPENING set only

**Load in stages. Do not bulk-load the family up front.** See `/craft` §A0
("Staged tool loading"); this section is its application to `/signals`.

The opening call loads the triage read surface and nothing else:

```
ToolSearch(
  query: "select:mcp__think-and-ship__signal_pending,mcp__think-and-ship__signal_status,mcp__think-and-ship__think_engine_status,mcp__think-and-ship__think_record_step",
  max_results: 4
)
```

Everything else is loaded **at the step that uses it**:

| Load at | Issue |
|---|---|
| Reading a specific signal | `select:…signal_get` |
| Churn — grounding | `select:…ministr_survey,…ministr_symbols,…ministr_definition,…ministr_read` — add `…ministr_toc`/`…ministr_references` only when the signal needs them |
| Churn — research | `select:mcp__serpapi__search` |
| Recording enrichment | `select:…signal_research,…signal_link` |
| Disposition — **only the verb you decided on** | `…signal_promote` / `…signal_surface` / `…signal_snooze` / `…signal_ignore` |
| Capturing something new | `select:…signal_capture` |
| Promotion to a chunk | `select:…roadmap_status,…roadmap_add_chunk` |
| Only when needed | `…think_get_step` |

The disposition verbs are the point: a triage pass that snoozes one signal
should never have loaded `signal_ignore` and `signal_promote`. Decide first,
then load the one verb.

What each tool is for (reference — this table is documentation, not a shopping
list; loading a tool is a separate, deliberate act):

- **`signal_*`** (the inbox you're working — native state):
  - `signal_status` — counts by lifecycle state + the inbox. Call first to see what's open.
  - `signal_get` — one signal by id (its enrichment trail + cross-refs).
  - `signal_capture` — record a new signal (kind, from, body).
  - `signal_research` — append a durable enrichment `{ think_step?, sources[], summary, confidence }` and advance `new`/`triaged` → `researched`. The churn workhorse.
  - `signal_link` — attach a `think:`/`task:`/`chunk:` cross-ref to a signal.
  - `signal_pending` — the ready-to-raise signals (researched, above-confidence, not surfaced, not snoozed), optionally filtered by relevance hints. The surface workhorse.
  - `signal_surface` — mark a signal surfaced (so it isn't re-raised).
  - `signal_snooze` — defer a signal for N minutes.
  - `signal_ignore` — dismiss a signal (terminal).
  - `signal_promote` — turn a validated signal into a backlog roadmap chunk (writes `chunk:` onto the signal and `signal:` onto the chunk). Idempotent.
- **`think_*`** — `think_record_step` records the reasoning behind an enrichment / a promote / a dismiss; pass its step number to `signal_research(think_step:)` so the signal cross-refs its reasoning.
- **`ministr`** — `ministr_survey` / `ministr_symbols` / `ministr_definition` / `ministr_references`: ground a signal in what the code actually does **before** judging it.
- **`serpapi`** — `serpapi_search`: validate a claim, find prior art, check the 2026 norm when a signal is a design question.
- **`roadmap_status`** — read the active chunk so surface mode can use it as a relevance hint.

If any server isn't available, note it and proceed — but flag the gap. `ministr` + `serpapi` are load-bearing for honest churn; skipping them turns enrichment into vibes.

## Mode A — CHURN (the default)

Pick up open signals and earn them a verdict. **One signal end-to-end per pass** so the user can interrupt.

1. **Read the inbox.** `signal_status` → pick the oldest `new` (or `triaged`) signal, or the one the user named. `signal_get` it for the full body + any prior enrichment.

2. **Open a think step.** `think_record_step` (step numbers are project-global — check `think_engine_status`): purpose "Churn signal <id> — <one line>", context = the signal body + what's already known, thought = the grounding plan. Capture its step number.

3. **Ground in the code (`ministr`).** Is the signal already addressed? Where would it live? `ministr_survey` the concept, `ministr_symbols`/`ministr_definition` the surface, `ministr_references` the blast radius. **This comes before serpapi** — verifying the premise in the real code prevents enriching a signal that's already moot.

4. **Research the world (`serpapi`)** when the signal is a design choice, a "best practice" claim, or a novelty — current-year results. Skip only for purely internal/mechanical signals (say so).

5. **Synthesise + record enrichment.** Close or extend the think step with the finding, then `signal_research(id, summary, confidence, sources[], think_step)`:
   - `summary` — what you found (grounded, specific).
   - `confidence` — 0..1, your honest read of how real/actionable this is.
   - `sources` — the ministr symbol ids + serpapi URLs you actually consulted.
   - `think_step` — the reasoning step, so the signal stays auditable.
   This advances the signal to `researched`.

6. **Verdict.**
   - **Validated + actionable** → `signal_promote(id)` (creates a backlog roadmap chunk; `/roadmap` will build it). Tell the user the chunk id.
   - **Refuted / already-done / not-worth-it** → `signal_ignore(id)` with the reason in the closing think step. An honest dismissal is a result, not a failure.
   - **Real but not yet** → leave it `researched`; it'll show up in surface mode when the work touches it.

7. **Close the think step** with the verdict + `pinned: true` if load-bearing.

## Mode B — SURFACE (earned interruption)

Raise the *right* signal at the *right* moment — fewer, higher-confidence interruptions, never nagging.

1. **Gather context.** What is the session touching? The active roadmap chunk (`roadmap_status` → `next`/in-progress), the files in play, the topic. These become **relevance hints**.

2. **Ask the engine.** `signal_pending(min_confidence, hints, limit)` — the engine returns only `researched`, above-threshold, not-already-surfaced, not-snoozed signals, filtered by your hints, highest-confidence first. It **never** returns an un-researched or low-confidence signal, so you can't nag with a guess.

3. **Raise at most one (the top one).** Present it to the user in a sentence: who raised it, what it is, why it's relevant now, the confidence + a link to its enrichment. Then `signal_surface(id)` so it isn't re-raised.

4. **Honor the user's reaction** (agent-inbox vocabulary):
   - *Act on it* → `signal_promote(id)` (→ roadmap chunk) or do it inline if trivial.
   - *Not now* → `signal_snooze(id, minutes)` — it disappears from `signal_pending` until it expires.
   - *Not worth it* → `signal_ignore(id)` (dismissed).
   - *Show me more* → `signal_get(id)` for the full enrichment trail.

5. **Stop.** Surface mode raises **one** earned interruption, then yields. If `signal_pending` is empty, say "nothing ready to raise" and move on — silence is the correct output when nothing has earned an interruption.

## Constraints and discipline

- **Native state is the source of truth.** Mutate via `signal_*` tools; the engine enforces the lifecycle, the confidence gate, and the snooze/surfaced bookkeeping — don't re-implement them.
- **Thin client.** The skill holds *procedure*, the engine holds *state*. Same split as `/roadmap` and `/roadmap-refresh`.
- **Tri-MCP or it didn't happen.** An enrichment without ministr grounding (and serpapi when it's a design question) is a vibe, not research. Cite real sources in `signal_research(sources:)`.
- **Honest negative findings.** A refuted signal gets `signal_ignore`d **with the reason recorded** in a think step — that's a real outcome.
- **Earned interruptions only.** Surface mode raises at most one signal per pass, only `researched` + above-threshold + relevant. Never raise a guess; never batch-nag.
- **One signal per pass.** Churn one end-to-end; surface one. So the user can interrupt.
- **Promotion, not implementation.** A validated signal becomes a roadmap chunk; this skill never builds the feature — that's `/roadmap`.

## Edge cases

- **No signals**: `signal_status` shows an empty/all-terminal inbox → report it and stop (offer `/signals capture` if the user has feedback to record). Local capture is the only inbound path today; collaborator/cloud submission is Phase 30 (not yet built).
- **Persistence disabled**: signals vanish between sessions if the server runs without `THINK_AND_SHIP_PERSIST=true`. Flag it; native state is in-memory only until enabled.
- **A signal can't be promoted** (`signal_promote` soft-errors): it must be `researched` or `surfaced` first — churn it (Mode A) before promoting.
- **`signal_pending` is empty in surface mode**: correct and common — nothing has earned an interruption. Say so; don't lower the threshold to manufacture one.
- **ministr / serpapi unavailable**: enrichment quality drops to code-only or memory-only — record a *lower* confidence and note the gap, don't claim grounding you didn't do.
- **The signal duplicates an existing roadmap chunk**: `signal_link(id, chunk:<id>)` to record the relationship, then dismiss or leave researched — don't promote a duplicate.

## Pairing with /roadmap, /roadmap-refresh, /loop

- **The flow**: `/signals` (churn → promote) → `/roadmap-refresh` (the promoted chunks reshape the plan) → `/roadmap` (build them). Surface mode runs *during* normal `/roadmap` work.
- **With `/loop`**: `/loop /signals` runs a churn pass on a self-paced cadence — one signal per tick so the user can interrupt.

## What this skill is NOT

- Not an implementer — promotion hands the work to `/roadmap`.
- Not a roadmap-reshaper — that's `/roadmap-refresh`.
- Not a notifier — it raises *earned* interruptions through `signal_pending`, not a feed.
- Not an ingestion endpoint — collaborator submission (webhook / GitHub Issues / email / web form) is the Phase-30 cloud backend, not this skill.
