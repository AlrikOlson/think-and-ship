# think-and-ship — Architecture

> **Reference** — facts to look up, not a reading path ([all docs](README.md)).

This document describes the server that runs. Where the shipped shape differs
from the shape it should have, that is stated as a divergence rather than
smoothed over — see [Divergences](#divergences) and [End state](#end-state).

Every structural claim here was checked against the source on 2026-08-12. The
tool counts, module sizes and dispatch paths are observations, not intentions.

**A note on cross-repository references.** Doc comments in the cloud and
tracker modules cite TypeScript counterparts by path — `backend/src/idempotency.ts`,
`backend/src/realtime.ts`, `frontend/src/tree/name.test.ts` and others. Those
files live in a separate, private repository that holds the hosted service;
this repository is the engine alone. The citations are kept because the
agreement they name is real: the two sides implement the same wire contract,
and `contract/` and `shared/` in this repository are the canonical tables both
are held to. Four `#[cfg(test)]` drift proofs here read those files directly —
see `src/cloud/envelope.rs`, `src/cloud/build.rs`, `src/roadmap/name.rs` and
`src/roadmap/region.rs`.

## Contents

- [The one-sentence shape](#the-one-sentence-shape)
- [Workspace](#workspace)
- [A family](#a-family)
- [Composition and dispatch](#composition-and-dispatch)
- [What is not a family](#what-is-not-a-family)
- [Persistence](#persistence)
- [Broadcast](#broadcast)
- [Where a record can go](#where-a-record-can-go)
- [MCP protocol surface](#mcp-protocol-surface)
- [Divergences](#divergences)
- [End state](#end-state)
- [Non-goals](#non-goals)

## The one-sentence shape

One binary serves four tool families over MCP; each family is a domain plus an
engine plus a wire adapter, and everything else in the crate is either
infrastructure they share or an outbound sink they feed.

## Workspace

Two members:

```
crates/
├── think-and-ship/          the library + the `think-and-ship` binary
└── think-and-ship-viewer/   Tauri desktop viewer (src-tauri + Svelte frontend)
```

The viewer depends on the engine crate for its wire types — `BroadcastFrame`,
`ThinkHistory`, `ThinkStep` — rather than keeping a second copy in step by
hand. That dependency is the reason the viewer pulls the server's transport
crates it does not use; see [Divergences](#divergences).

## A family

Four families exist: `think_*` (reasoning), `ship_*` (execution), `roadmap_*`
(the long-horizon plan the other two serve), `signal_*` (what stakeholders
raised). Each is a directory under `src/` with the same internal shape:

| Part | What it is |
|---|---|
| `domain/` | Plain records. No logic, no IO, no MCP types. The viewer deserializes these bytes. |
| `engine/` | All behaviour. Drivable from a test with no wire and no disk. |
| `mcp/` | The rmcp adapter: `#[tool]` methods, arg types, the family's `initialize` instructions. |
| `output_schemas/` | JSON Schema for each tool's `structuredContent`. |
| `broadcast.rs` | The family's frame type for the live socket. |

The regularity is the point: a fifth family is a fifth directory of that shape
plus one arm in `Family::ALL`, and nothing else in the crate has to know.

`signal/` is the least built out — `domain.rs` and `engine.rs` are flat files
where the others are directories. That is a size difference, not a structural
one.

## Composition and dispatch

`mcp::unified::UnifiedService` holds one `Arc` per family service and is the
only dispatcher. It routes an incoming `tools/call` by name prefix, and the
listing and the routing must agree — a prefix the router lists but `route_of`
does not claim is advertised-but-uncallable, which is a bug this codebase has
already shipped once.

`FamilySelection` decides which families a deployment exposes. It is resolved
once at startup and fixed at construction, so it cannot drift per request; a
think-only deployment serves a correspondingly smaller tool surface.

`Family::ALL` is the single list of families. Everything that *describes* the
surface — the `initialize` instructions, the generated `CLAUDE.md` — iterates
it rather than restating it, because a hand-maintained count is a count that
goes stale. This crate's own front page claimed two families when there were
four.

One name is intercepted before routing: `ship_ship` is answered with
`ship_finalize`. It is not a legacy alias — it is the name a caller derives
from the `ship_` prefix instead of reading, and answering it costs one branch
against a retry over 53 candidates.

## What is not a family

Roughly two thirds of the crate. These are subsystems the families use or feed:

| Module | Lines | Role |
|---|---|---|
| `cli/` | 18.9k | Every subcommand: `serve`, `init`, `project`, `skills`, `tracker`, `connect`, `sync`, `otel`, … |
| `tracker/` | 15.6k | A provider-agnostic port that mirrors the roadmap out. Registered adapters: `github`, `github_projects`, `linear`. |
| `cloud/` | 6.0k | Sync the local store with the per-tenant backend. |
| `infra/` | 3.7k | Shared: persistence, project identity, cross-refs, broadcast, repo sync. |
| `mcp/` | 3.0k | The wire adapter, plus caching, progress, elicitation, resources, tasks. |
| `otel*`, `telemetry/` | 3.9k | OpenTelemetry export, live emission, consent, scrubbing. |
| `corpus/`, `usage.rs`, `hygiene.rs`, `content.rs` | 2.6k | Eval corpus, call counts, redaction, structured bodies. |

For comparison the four families total 24.4k, of which `roadmap/` alone is
12.4k. `cli/` is the single largest module in the crate.

`tracker_*` is not a family. It is a second prefix CLAIMED by `Family::Roadmap`
— the namespace through which the plan is mirrored — because the state it moves
is roadmap state. `Family::prefixes` is the one table that records the claim,
and routing, the operator-typed family selection, the refusal that lists known
families and the `initialize` block all derive from it. Nothing restates it.

## Persistence

Off by default; `THINK_AND_SHIP_PERSIST=true` turns it on.

```
~/.local/share/think-and-ship/
├── think/sessions/<project_id>.json
├── ship/sessions/<project_id>.json
└── roadmap/sessions/<project_id>.json
```

Every family keys its store by one `project_id`, resolved from a caller-supplied
override, then `THINK_AND_SHIP_PROJECT_NAME`, then `.think-and-ship/project.json`
found by walking up from the cwd, then a deterministic `<basename>-<6hex>`
fallback. A repository that declares its identity gives all of its worktrees the
same id.

**The durability model.** Several server processes — one per agent session —
can share one project's state file. A plain overwrite from a process holding
stale memory erases mutations another process already acknowledged. Every save
therefore takes an exclusive OS advisory lock on a sibling `.lock` file,
re-reads the on-disk state, merges it into memory through a family-specific
merge, and writes atomically (`write tmp; rename`). A `schema_version` mismatch
reads as absent rather than being reinterpreted.

This model is implemented **twice** — see [Divergences](#divergences).

## Broadcast

One NDJSON-over-Unix-socket stream, family-tagged, so a single viewer
interleaves every family:

```
{ "family": "think", "type": "step_appended",    ... }
{ "family": "ship",  "type": "action_recorded",  ... }
```

The accept loop must push a client into the fan-out list **before** publishing
the subscriber count. Reversed, a waiter wakes on a client that is not yet
reachable and the frame is dropped — a few instructions wide, so it fails
probabilistically and was originally a CI-only flake. Behavioural tests cannot
see it; two source gates in `infra/broadcast.rs` pin the ordering and pin that
every test which connects also waits for registration.

## Where a record can go

`SyncTarget` selects the sink, and the local store is always written:

- `Local` — per-user XDG persistence only.
- `RepoGit` — additionally mirror into the repo's `.think-and-ship/` as Agent
  Trace JSONL, one commit per *session*, never per step. At 100 sessions/day ×
  5 devs × 50 steps, per-step commits would be ~25k/day. Local and shared
  partitions are separate and the default is private.
- `Cloud` — write-through push to the per-tenant backend, which is the
  system-of-record for signals. Wired only when selected *and* the cloud URL
  and token are both set.

OpenTelemetry is a fourth outbound path, independent of `SyncTarget`: a receipt
lane, a live span lane, and a log lane, each off unless an endpoint is
configured.

## MCP protocol surface

Targets MCP `2025-11-25` via rmcp `3.0.0-beta.2`, advertised on `initialize`
over both stdio and Streamable HTTP.

Two extensions ride one seam each in `UnifiedService::call_tool`, so every tool
in every family gets them without knowing they exist:

- **Trace context (SEP-414)** — rmcp lifts the wire `params._meta` into
  `context.meta` before dispatch; the adopted context is written to the otel
  partition, collapsed to one write per trace rather than per call.
- **Long gates as tasks (SEP-2663)** — a client that declared the tasks
  capability gets a task handle for a long-running gate instead of a blocking
  call; a client that declared nothing still gets the blocking result.

Liveness progress rides the same seam, started after the interception branch,
and emits nothing at all for a call that finishes quickly.

## Divergences

Where the shipped shape differs from the shape it should have. Each is a real
finding with a measurement, not a style preference.

### D2 — one family still recomputes the durability wiring

`infra::persistence::Persistence` serves roadmap, signal, the trace context and
the usage counters through a `Domain` enum, and now serves think as well: that
family keeps a wrapper holding its merge policy, its versioned envelope and its
per-project file naming, but the lock, the atomic write and the partition path
are the shared implementation's.

`ship` has not been folded. It declares its own `Persistence` and its own
`PersistenceConfig`, recomputing the partition path (`data_dir/ship/sessions`)
and the environment resolution that `Domain::Ship` and
`PersistenceConfig::from_env` already provide, and borrows only
`locked_merge_write` and `default_data_dir` from the shared module. It is a
smaller divergence than the one it is left over from — the mechanism is already
shared — but it is the last place where a second answer to "where does this
family's data live?" is computed.

Measured while folding think, both since fixed: the two config resolutions had
drifted apart, so `THINK_AND_SHIP_PERSIST=1` persisted every family except
think, and a set-but-blank `THINK_AND_SHIP_DATA_DIR` resolved to the platform
root in one resolver and to the empty path in the other.

### D3 — one family's modules sit a level above everyone else's

`think/` has `config.rs`, `constants.rs`, `formatter.rs` and `util/` at its top
level, and no other family has an equivalent. Checked against the construction
sites on 2026-08-12, this is a PLACEMENT difference and not, as previously
recorded here, standalone-server baggage:

- `formatter.rs` is the renderer behind a live tool — `think_export_trace`'s
  handler dispatches into it for markdown, json, console and the branch tree.
- `constants.rs` is imported by the engine's validation, session and process
  modules; `util/text.rs` by four engine modules.
- `config.rs` still carries the validation, feature, display, system and
  broadcast settings the engine reads. Its persistence half was the duplicated
  part, and that is gone.

What remains true is that these are the family's own domain material filed as
siblings of `domain/` and `engine/` rather than inside them. The end state's
phrasing — that this config "folds into the shared configuration" — presumes a
shared configuration module, and there is none: `infra/` has persistence,
broadcast, cross-refs and project identity, and no config. Closing this means
either creating that module for every family or accepting the layout, and that
is a decision rather than a cleanup.

Removed while checking: `Formatter::plain`, which had no caller anywhere
including tests despite a doc comment directing MCP responses at it, and
`format_history_summary`, 69 lines held up by six tests and called by nothing.

### D4 — sixteen verbs exist only on the command line

18.9k lines against 24.1k for all four families combined, with `cli/mod.rs`
alone at 6.1k. Audited on 2026-08-12 by taking the command tree from the
argument parser and checking each leaf against the actual tool surface. All 50
leaf commands, classified:

| | count | what they are |
|---|---|---|
| **Stay** | 27 | `serve`; the config writers and installers (`init`, `skills *`, `project mark`); credentials and connection (`connect`, `disconnect`, `token`, `tracker connect/sign-in/disconnect`); the local telemetry stack (`otel *`); the consent switches (`telemetry *`); machine-local reads (`doctor`, `status`, `calls`); and the two file-moving trace verbs (`trace export`, `trace promote`) |
| **Thin adapter** | 7 | `roadmap export/next/status/block/unblock`, `tracker status/setup` |
| **Missing tool** | 16 | below |

The sixteen are the divergence. By the rule in
[End state](#end-state) item 4, a command doing something no tool can is a
missing tool, not a reason for logic in the adapter:

- **Tracker lifecycle (6)** — `include`, `exclude`, `on`, `off`, `push`,
  `pull`. An agent can set mirroring up and ask its status, and can then do
  nothing with it. This is the one that matters. The `tracker_*` namespace has
  two tools not because two is its size but because that is where the surface
  stopped being built; the port behind it serves eleven command-line verbs.
- **Store custody (4)** — `prune`, `adopt`, `roadmap prune`, `repair`. What a
  project's store contains, and the repair of a duplicated trace, are decided
  by verbs the agent filling that store cannot run.
- **Roadmap (3)** — `import`, `regions`, `hygiene`. `hygiene` writes signals,
  so the sweep meant to notice neglected work only starts from a terminal.
- **Corpus and back-fill (3)** — `corpus export`, `corpus eval`, `sync push`.
  At least `corpus eval` may be legitimately developer-facing; that argument
  should be recorded rather than assumed either way.

The line count is not itself the finding. 27 of 50 commands are exactly what a
command line is for, and the module is large partly because that work is real.

### D6 — the viewer pays for 85 crates it has no use for

Re-priced on 2026-08-12. It is not four wire types. The viewer imports two
broadcast frame types, three domain types, `Persistence` + `read_history`, and
`ReasoningServer` — but every use of the engine goes through
`ReasoningServer::for_analysis`, the constructor that takes a history and
branches and opens nothing, behind three commands: impact, checkpoint, search.
So the surface is domain + persistence + broadcast + the ANALYSIS half of the
engine, and none of the transport.

**Measured:** the viewer resolves 314 unique crates; **85 of them are reachable
only through the engine crate** — 27% of its build, including `clap`, `axum`
and an authenticated-encryption stack, in a desktop app that opens a Unix
socket and reads JSON. Binary size was NOT measured: that needs a release build
of a "without" configuration that does not exist yet.

**Decided: extract.** `domain` + `persistence` + `broadcast` + the analysis
engine become a small crate both depend on. A feature flag was rejected — it
would have to gate the transport out of a crate that also ships a binary
requiring it, and would leave the duplication below untouched.

The cost is also not being spent on what justified it: the viewer hand-copies
the data-directory resolution rather than calling the crate it already depends
on. That is the fourth copy of that logic, and the one the persistence work did
not reach.

*Superseded reason, kept for the record:* depending on the engine crate for
four wire types pulls axum, reqwest, rmcp and
tokio-tungstenite into a desktop app that opens a Unix socket and reads JSON.
Correct, and cheaper than a second copy of the types, but the cost is real.

## End state

The target is not a redesign. It is the shape the code already mostly has, with
the two remaining things that duplicate it removed and the two that outgrew
their placement moved.

**1. One family abstraction, and it is the rmcp service. — held.** The
extension point is a directory of the shape in [A family](#a-family) plus an arm
in `Family::ALL`. There is no second, transport-agnostic dispatch layer: the one
that existed never dispatched anything in production, and the abstraction it
offered — unit-testing a family without a transport — is already available by
driving the engine directly, which is what every family's tests actually do.
Adding one back means adding a dispatch path no serve path builds.

**2. One persistence implementation.** `infra::Persistence` with a `Domain` per
family, and every family on it. The merge semantics stay per-family, because
merging two histories is not merging two roadmaps; the locking, the atomic
write and the partition path are one implementation with one incident history.
A family's persistence module holds policy — its merge, its envelope, its file
naming — and no mechanism. `think` is there; `ship` is the one left.

**3. Every family files its own material the same way.** `think` keeps a
config, constants, a formatter and text utilities that no other family has as
top-level modules — but each is live, so the target is a consistent layout, not
a removal. Reaching it needs a decision this document should not pre-empt:
either `infra/` grows a configuration module every family uses, or the material
moves inside `domain/`/`engine/` and the layout rule is written down. What is
already settled is the negative: none of it is a second implementation of
something shared, and none of it should be deleted for looking unusual.

**4. The CLI becomes an adapter.** Every subcommand is a thin shell over an
engine verb, held to the same rule the MCP adapter follows: no behaviour lives
in the adapter. Where a command does something no tool can, that is a missing
tool, not a reason for logic in `cli/`. The config-writing and installer
commands (`init`, `connect`, `skills`) are genuinely CLI-shaped and stay as
they are.

**5. `tracker` is an explicit port behind roadmap.** Settled. Not a fifth
family — mirroring really is roadmap state — and not a special case either:
`Family::Roadmap` CLAIMS `tracker_*` as a second prefix, `Family::prefixes` is
the one table that records it, and `route_of` derives from that table instead
of naming prefixes one at a time. The `initialize` block is held to the same
table by a gate, so it can no longer advertise a namespace nothing claims. The
15.6k lines behind it are named as what they are: a provider-agnostic port with
adapters registered for `github`, `github_projects` and `linear` — Jira is
scaffolded (ADF bodies, Atlassian credentials) but unregistered, and the module
doc is bound to `registry::PROVIDERS` so that list cannot drift again. What
remains open is the SIZE of the namespace, not its shape: six mirroring verbs
still exist only on the command line, which is item 4's problem rather than
this one's.

**6. The viewer depends on a crate its size.** `domain` + `persistence` +
`broadcast` + the analysis engine are extracted into a small crate the server
and the viewer both depend on, and the viewer stops copying what that crate
already resolves. Measured at 85 of its 314 crates today.

**7. Outbound sinks share one seam.** Local disk, repo git, cloud and OTLP are
four independent paths out of the same records today. One sink seam with four
implementations means a fifth destination is an implementation rather than a
fifth traversal of the record set.

**The rule that keeps this document true.** Every divergence above is an
artifact that recorded an *intention* — a design contract, a trait, a second
implementation — and was never re-derived from what runs. Tests do not catch
it, because tests written against the losing design pass forever. So: this
document describes the running system, dates its observations, and states
divergences as divergences. When one is closed, it is deleted from here. When
a claim here can be checked by a gate, prefer the gate.

## Non-goals

- **A plugin system.** Families are compiled in. `FamilySelection` decides which
  are served, and that is the whole of the configurability.
- **A second transport abstraction.** rmcp is the MCP layer. stdio and
  Streamable HTTP are its transports.
- **Backwards compatibility with pre-`0.3.0` on-disk state or tool names.**
  There is none, deliberately.
