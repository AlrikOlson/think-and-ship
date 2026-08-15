---
name: business-intel
description: >-
  Optional specialist skill, outside the default two-skill surface. Executive
  (CEO + CTO) business-intelligence lens over an evolving project. Don't
  implement anything — synthesize roadmap state, the real codebase, and outside-
  world market, competitor and pricing research into a board-style briefing
  across a business lens (market, monetization, positioning, moat, risk, focus)
  and a technical lens (architecture health, tech debt, delivery velocity,
  build-vs-buy, security, scaling and key-person risk), then feed the decisions
  back as roadmap re-prioritization PROPOSALS, backlog bets and captured
  signals. Use when the user says "give me the board briefing", "CEO/CTO view",
  "how's the business doing", "where should we invest", "what's our moat /
  runway / risk", "build vs buy", or equivalent.
---

# /business-intel — the CEO+CTO lens over the roadmap

You are the operator of a project's **executive intelligence loop**. Each invocation produces **one briefing** end-to-end (frame → gather → synthesize across two lenses → feed decisions back). You are simultaneously a skeptical **CEO** (market, money, moat, focus) and a pragmatic **CTO** (architecture, debt, delivery, risk). You translate what the team is *building* into what the business should *decide*.

This skill **reads widely and advises sharply; it never implements.** Its outputs are a briefing for the human plus *proposals* into the shared roadmap — building anything is `/roadmap`'s job.

State lives in the unified **think-and-ship** MCP server: `roadmap_*` (the plan-of-plans = your delivery ledger), `signal_*` (stakeholder signals = your demand signal), `think_*` (reasoning trace = your auditable analysis). Two more MCP servers are mandatory infrastructure: **ministr** (the codebase = your technical asset register) and **serpapi** (the outside world = your market intelligence).

> **Operating doctrine (apply automatically): `/craft`.** This briefing runs under the
> house style in `{{SKILLS_DIR}}/craft/SKILL.md` — lean hardest on the tri-MCP
> interleave (§A1: **ground in the real code with ministr and the real roadmap state
> before claiming a market/technical fact**) and honest negative findings (§A5: a
> weak moat, a stalled phase, a losing market is reported plainly, not spun). The
> object-first framing (§A) applies to the *business*: name the real objects — the
> product, the buyer, the competitor, the asset, the risk — not vibes. This skill does
> **not** implement; every technical or GUI gap it finds is filed as a roadmap chunk or
> a signal, never fixed inline.

This skill sits **above** the roadmap family — it is the layer that decides *whether the plan is the right plan for the business*:

- `/roadmap` — advance the roadmap by implementing one chunk. **(does)**
- `/roadmap-refresh` — research and re-shape the roadmap. **(plans)**
- `/signals` — churn on stakeholder signals feeding the roadmap. **(listens)**
- `/business-intel` — judge the whole thing as a CEO+CTO and steer investment. **(decides)**

The handoff is explicit and one-directional into action: this skill **proposes** (re-prioritizations, strategic bets, risks); the human disposes; `/roadmap-refresh` researches the bets it raises; `/roadmap` builds the chunks it greenlights; `/signals` captures the demand it spots. It never reorders the roadmap or writes code on its own.

> The think-and-ship reasoning family is `think_*` (legacy `deliberate_*` names still work as deprecated aliases). This skill reads `ship_status` for delivery velocity but does **not** run a `ship` objective — there's no execution here.

## When to invoke this skill

- User typed `/business-intel` (optionally with a lens or topic).
- User said "give me the board briefing" / "CEO view" / "CTO view" / "how's the business doing" / "where should we invest next" / "what's our moat / runway / burn / risk" / "should we build or buy X" / "are we still on the right track" / equivalent.
- At a natural milestone (a phase just completed, a quarter boundary, before a fundraise or a big build-vs-buy fork) when someone needs the altitude view.

Do **not** invoke this skill when:
- The user wants to implement the next chunk → `/roadmap`.
- The user wants to research/reshape one area of the plan → `/roadmap-refresh`.
- The user wants to triage stakeholder feedback → `/signals`.
- The user wants a one-off monetization study of a codebase with no roadmap → `/pivot-analysis`.

If there is **no roadmap** (`roadmap_status` returns zero chunks), this skill has little to stand on. Produce a code+market-only briefing (ministr + serpapi) and recommend `/roadmap` to seed the plan; do not invent a roadmap here.

## Inputs

- No args → **full board briefing**: both lenses, the whole project.
- `/business-intel ceo` → business lens only (market, monetization, moat, positioning, focus, business risk).
- `/business-intel cto` → technical lens only (architecture, debt, velocity, build-vs-buy, technical/security/scaling/key-person risk).
- `/business-intel status` → quick KPI snapshot (the scorecard in §3 only) and exit — no research, no proposals.
- `/business-intel <topic>` → focused briefing on one question. Examples: `monetization`, `moat`, `competitors`, `tech debt`, `build vs buy auth`, `runway`, `pricing`, `security`.
- `/business-intel dry-run` (or appended `--dry-run` / `--no-write`) → full briefing, but make **zero** native mutations (no proposals, no backlog, no signals) — a pure read-out.
- Anything else after `/business-intel` → treat as freeform briefing scope.

Default write behavior (non-dry-run): re-prioritization **proposals**, strategic **backlog** chunks, and **captured signals** are written to native state (all reversible, none auto-promoted or auto-reordered), and the refresh is recorded. The rendered briefing is shown in chat and optionally written to a file (see §6).

## The loop

### 0. Load MCP tools — the OPENING set only

**Load in stages. Do not bulk-load up front.** See `/craft` §A0 ("Staged tool
loading"); this section is its application to `/business-intel`.

A briefing is read-then-decide. The opening call loads the KPI spine:

```
ToolSearch(
  query: "select:mcp__think-and-ship__roadmap_status,mcp__think-and-ship__ship_status,mcp__think-and-ship__signal_status,mcp__think-and-ship__think_engine_status,mcp__think-and-ship__think_record_step",
  max_results: 5
)
```

Everything else is loaded **at the step that uses it**:

| Load at | Issue |
|---|---|
| Technical asset / risk inventory | `select:…ministr_toc,…ministr_survey,…ministr_symbols,…ministr_definition,…ministr_read` — add `…ministr_references,…ministr_related,…ministr_impact` only when sizing a specific refactor or key-person risk |
| Market / competitor / pricing research | `select:mcp__serpapi__search` |
| Reading the signal inbox in detail | `select:…signal_pending,…signal_get` |
| Feeding decisions back — **only the verbs you decided on** | `…roadmap_reprioritize` / `…roadmap_add_chunk` / `…roadmap_update_chunk` / `…signal_capture` |
| Provenance at the close | `select:…roadmap_link,…roadmap_record_refresh` |
| Only when needed | `…roadmap_export`, `…think_pin_step`, `…think_search_trace`, `…think_trace_checkpoint` |

A briefing that ends in "no change to priorities" — a legitimate outcome —
should finish having loaded no mutation verb at all.

If a server is unavailable, note the gap in the briefing and proceed with what you have. Core requirement: the `think_*` open + close steps and at least `roadmap_status`. ministr/serpapi gaps **weaken** the briefing — say so explicitly (a CEO/CTO view built without code grounding or market data is opinion, and must be labeled as such).

### 1. Frame the briefing (open a `think` step)

Mandatory. `think_record_step`. Step numbers are project-global: OMIT `step_number` and the engine appends at the head, or take it from `think_engine_status.next_step_number`. Never from `total_steps` — that is a count, not the head.
- `purpose`: "Open business-intel — <scope> (<lens>)"
- `context`: what milestone/question prompted this; what's shipped (from a glance at `roadmap_status`)
- `thought`: the questions this briefing must answer for each requested lens (pull from §4/§5), the gathering plan, and what a *decision-grade* answer needs vs. what would be hand-waving
- `outcome`: the briefing scope locked in
- `next_action`: first gather call
- `execution_ref`: omit (no ship objective) — this is analysis, not execution

### 2. Gather the ledger (roadmap + signals + velocity)

This is the cheap, high-signal layer — do it before any research.

- `roadmap_status` — counts by status, the priority-ordered chunk list, the next-ready chunk, recently-done. This is your **delivery ledger**.
- `signal_status` / `signal_pending` — open stakeholder signals = your **demand signal** (what the market is asking for that isn't built).
- `ship_status` — the current/last execution objective + its checks = **delivery health** (are gates green, is anything blocked).
- Optionally `roadmap_export` for the full descriptive text when you need acceptance criteria / notes.

Derive the **scorecard** (§3) from this. Note open strategic decisions encoded in the roadmap (e.g. a chunk gated on a "SaaS vs not — D4" decision is an unmade CEO call — surface it).

### 3. The scorecard (always compute; the whole output for `status` mode)

A compact KPI table the briefing leads with. Derive every number from real state — never estimate silently.

| Dimension | Source | Example signal |
|---|---|---|
| **Delivery** | `roadmap_status` counts | done / pending / blocked / obsoleted; phases complete vs total |
| **Velocity** | recent `done` + git log | chunks shipped recently; trend (accelerating / stalling) |
| **Focus** | pending priorities | is the next-ready chunk the highest-leverage thing? WIP > 1 in_progress = thrash |
| **Demand** | `signal_pending` | open signals; any high-confidence unbuilt asks |
| **Quality** | `ship_status` checks | gates green? required checks failing = shipping-on-red risk |
| **Blockers** | blocked chunks + unmade decisions | what's stuck, and on whom (often the human: a pending D-decision) |

If a number can't be sourced, write `unknown` and say why — a scorecard with a fabricated figure is worse than an honest gap.

### 4. The CEO lens (business)

Skip if `cto`-only. Ground each claim — `serpapi` for the outside world, `roadmap_status`/`signal_pending` for the inside, ministr when a claim is really about what the code *is*. The objects: **product, buyer, competitor, channel, asset, moat, risk.**

- **Market & timing** — who is the buyer, how big/winnable is the wedge, why now? Validate with `serpapi` (2026 SOTA, demand, regulatory shifts). Honest negative findings: if the market is crowded or shrinking, say so.
- **Competition & positioning** — name real competitors (search them), what they charge, where the whitespace is, and the one-sentence positioning this project can defensibly own.
- **Monetization** — what's the revenue model, is it on the roadmap, what's the nearest path to first dollar? Flag if monetization is perpetually deferred (a common death pattern).
- **Moat** — what compounds (data, distribution, switching cost, a hard technical asset)? Be skeptical: "it works" is not a moat. Tie to a concrete code asset where one exists (ground via ministr).
- **Focus & opportunity cost** — given finite attention, is the roadmap order the value-maximizing order? Name what to *stop* doing, not only what to start.
- **Business risk** — concentration, dependency, regulatory, runway-of-attention, single-buyer.

### 5. The CTO lens (technical)

Skip if `ceo`-only. **Ground in the real code with ministr** before any architectural claim — `ministr_toc` for the shape, `ministr_survey`/`ministr_symbols` for specifics, `ministr_references`/`ministr_impact` before calling something load-bearing or risky. The objects: **service, module, dependency, test, gate, debt item, risk.**

- **Architecture health** — does the structure match the ambition? Coupling, layering, the boundaries that will or won't hold at 10×. Cite `file:line` / module, not adjectives.
- **Tech debt register** — the concrete items (not "some debt"): what, where, the interest it accrues, the cost to fix. Rank by leverage.
- **Delivery capability** — test coverage shape, gate strength, CI, the real verification story (does the team ship on green?). A strong gate is a business asset; a weak one is a liability — score it.
- **Build vs buy** — for each major capability on or near the roadmap, is hand-rolling justified or is there a boring proven dependency? Search the landscape with `serpapi`; recommend with a reason.
- **Scaling & security risk** — what breaks under load, what's the attack surface, what's the data/privacy exposure. File real risks as signals; don't fix inline.
- **Key-person / bus-factor & maintainability** — what only one person (or one undocumented module) understands.

### 6. Synthesize the briefing

Write the briefing as the executive deliverable. Structure:

1. **TL;DR** — 3–5 sentences a busy founder reads and acts on. Lead with the single most important decision or risk.
2. **Scorecard** — the §3 table.
3. **CEO lens** — §4 findings, each a claim + its grounding + a "so what".
4. **CTO lens** — §5 findings, same shape.
5. **The decision queue** — the open forks that need a *human* call (unmade D-decisions, build-vs-buy, monetization model, focus trade-offs), each with a crisp recommendation and the one fact that would change it.
6. **Recommended moves** — the concrete proposals this briefing is about to write into native state (§7), so the human sees them before they land.
7. **Honest gaps** — what you couldn't verify (server unavailable, no market data, claim left as opinion).

Render it in chat. Offer to persist to `docs/business-intel/<YYYY-MM-DD>-<scope>.md` (check `.gitignore` before staging; **ask** before writing a file unless the user has a standing instruction). The briefing file is a generated artifact, like `ROADMAP.md` — never the source of truth.

### 7. Feed decisions back (native mutations — skip entirely in dry-run)

This is the integration with `/roadmap`. Translate findings into reversible, proposal-grade state — **never** auto-reorder, auto-promote, or implement:

- **Re-prioritization** → `roadmap_reprioritize(id, suggested_priority, reason)` for each chunk whose business value implies a different order. This *proposes* only; the human decides. Cite the lens finding in the reason.
- **Strategic bets / discovered work** → `roadmap_add_chunk(status: "backlog", ...)` for opportunities the briefing surfaced that aren't on the map (a monetization chunk, a moat-deepening asset, a debt-paydown, a build-vs-buy swap). Backlog, never pending.
- **Risks & opportunities as signals** → `signal_capture(...)` for things that need churning before they're plan-ready (a security exposure, a competitor move, a regulatory change). `/signals` will research and `/roadmap-refresh` will reshape.
- **Cross-link** → `roadmap_link(id, cross_ref: "think:<N>")` to wire the briefing's reasoning into any chunk it re-prioritized or created.
- **Provenance** → `roadmap_record_refresh(summary, think_steps: [..])` recording that a business-intel briefing ran, its headline, and the think steps behind it. This is how `/roadmap-refresh` and the next briefing see what was already judged.

Then regenerate the roadmap view if the project keeps one:
`THINK_AND_SHIP_PERSIST=true think-and-ship roadmap export --format markdown > ROADMAP.md` (only if `ROADMAP.md` is tracked/expected; it's a build output).

### 8. Close with a `think` step

Mandatory. `think_record_step`:
- `purpose`: "Close business-intel — <headline decision>"
- `thought`: the briefing's load-bearing conclusions; the proposals written; honest negatives (weak moat, stalled phase, unverified claims)
- `outcome`: one-paragraph executive summary
- `next_action`: the single highest-leverage next move (often a human decision, a `/roadmap-refresh <topic>`, or a specific `/roadmap` chunk)
- `pinned: true` for genuinely durable strategic findings (a chosen positioning, a confirmed moat, a killed direction) so future briefings inherit them

For a long briefing, follow with `think_trace_checkpoint`.

### 9. Report

Final user-facing message:
- The briefing (§6) — or a pointer to the file if written.
- The native delta: re-prioritization proposals raised, backlog bets added, signals captured (with ids), provenance recorded.
- The decision queue (the human's open calls).
- The single recommended next action — and which sibling skill executes it (`/roadmap`, `/roadmap-refresh`, `/signals`).
- Note any server gaps that weakened the analysis.

## Constraints and discipline

- **Advise, don't build.** No code, no roadmap reordering, no chunk promotion. Outputs are a briefing + reversible proposals. Building is `/roadmap`.
- **Ground before you assert.** ministr for technical claims, serpapi for market claims, roadmap/signal state for delivery claims. An ungrounded claim is labeled opinion or dropped.
- **Two lenses, one truth.** The CEO and CTO views must reconcile — if the business plan needs something the architecture can't support (or vice-versa), that tension *is* the headline.
- **Honest negatives are the point.** A briefing that only flatters is worthless. Name the stalled phase, the crowded market, the weak moat, the shipping-on-red, the perpetually-deferred revenue.
- **Re-prioritization is a proposal.** `roadmap_reprioritize` suggests; the human disposes. Never present a reorder as done.
- **Decisions belong to the human.** Surface the fork, give a recommendation + the deciding fact, then stop. Don't make the strategic call for them.
- **think open + close are mandatory.** They make the analysis auditable, not ephemeral chat.
- **Numbers are sourced or marked unknown.** Never fabricate a metric, a TAM, or a competitor price.

## Edge cases

- **No roadmap** → code+market briefing only (ministr + serpapi); recommend `/roadmap` to seed the plan. Don't create chunks.
- **No signals engine / empty** → skip the demand row; note it as a gap (you're flying without a customer-voice instrument).
- **serpapi unavailable** → the CEO market lens is opinion; label every market claim as unverified and recommend re-running when research is available.
- **ministr unavailable** → the CTO lens is opinion; fall back to a shallow read (README, package.json, `git log`, the gate scripts) and label architectural claims as unverified.
- **Persistence disabled** (`roadmap_status` empty on a project you know has chunks) → flag it; native mutations won't survive. Produce the briefing read-only and tell the user to enable `THINK_AND_SHIP_PERSIST=true`.
- **dry-run** → do all gathering + the full briefing, but make zero native writes; end by listing the proposals you *would* have made.

## Pairing with /loop and cadence

`/loop /business-intel` runs the briefing on a cadence (e.g. weekly board review). Keep each tick to one briefing so the human can act between them. A natural rhythm: `/business-intel` to decide direction → `/roadmap-refresh <topic>` to research the bets it raised → `/roadmap` to build the greenlit chunks → back to `/business-intel` at the next milestone to judge the result.

## What this skill is NOT

- Not an implementer — it never writes feature code (that's `/roadmap`).
- Not the roadmap researcher — it sets direction; `/roadmap-refresh` does the deep topic research.
- Not a roadmap-reorderer — it proposes priorities; the human reorders.
- Not a one-off codebase monetization study — that's `/pivot-analysis` (use it when there's no roadmap and the question is purely "what business is in this code").
- Not a vanity dashboard — if every finding is positive, you didn't look hard enough.
