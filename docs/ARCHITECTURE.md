# think-and-ship — Architecture

> **Status:** target design for `think-and-ship` v0.2.0 (the unified server).
> **Current shipped layout** (v0.1.x): two cooperating MCP servers —
> `deliberate-mcp` and `resolute-mcp` — with a shared `think-and-ship-core`
> crate. The merge has not yet been implemented. See
> [Why merge](#why-merge) for the rationale, and the v0.1.x section of
> the [README](../README.md) for the layout you'll see in `git log` today.

This document is the contract for the merge. It is the answer to "what
does a single `think-and-ship` server look like, and where does each
piece live?" without having to read source.

---

## Status

- **Spec version:** MCP `2025-06-18` (via rmcp 1.7)
- **Next migration target:** `2026-07-28` RC — pending [rust-sdk#526](https://github.com/modelcontextprotocol/rust-sdk/issues/526)
- **Deprecation window:** ≥12 months per SEP-2596 — current server stays valid against future clients

## Table of contents

1. [Why merge](#why-merge)
2. [Crate layout](#crate-layout)
3. [Module layout (inside `crates/think-and-ship/src/`)](#module-layout)
4. [The `ToolFamily` trait](#the-toolfamily-trait)
5. [Typed `CrossRef`](#typed-crossref)
6. [Subcommand binary](#subcommand-binary)
7. [Persistence and broadcast (unified)](#persistence-and-broadcast)
8. [Environment variables](#environment-variables)
9. [Migration story](#migration-story)
10. [SOLID checklist](#solid-checklist)
11. [Non-goals](#non-goals)
12. [Sources](#sources)

---

## Why merge

`deliberate-mcp` records reasoning. `resolute-mcp` records execution.
They cross-reference each other (`execution_ref` on a step,
`deliberate_step` on an action) and share a project identity algorithm
in `think-and-ship-core`. In practice they are never used independently
— installing one without the other immediately produces dangling
cross-references.

Three signals point the same way:

- **AWS DDD guidance on MCP server boundaries**: *"server boundaries
  should reflect agent responsibility, not tool availability."* (See
  [Sources](#sources).) Both halves serve one agent responsibility:
  reason and ship.
- **The cost is coordination, not code.** A `ministr_solid` audit of
  the v0.1.x workspace finds zero SOLID violations. The merge is not
  refactoring tech debt; it removes two MCP server blocks per project,
  two env-var families, two persistence directories, and two broadcast
  sockets that always travel together anyway.
- **We already own the name.** `think-and-ship` is published on both
  npm and crates.io. The rename phase that was originally Phase 14a in
  the roadmap is obsoleted.

The merge is *not* a rewrite. The two engines move under one roof
behind two tool family namespaces (`think_*`, `ship_*`); the wire
contract for old tools is preserved as deprecated aliases for one
release.

---

## Crate layout

Post-merge workspace:

```
think-and-ship/
├── crates/
│   ├── think-and-ship/         — the unified MCP server (lib + bin)
│   └── think-and-ship-viewer/  — Tauri desktop viewer (renamed from deliberate-app)
└── npm/
    └── think-and-ship/         — single npm package: binary + CLI + MCP server
```

Eliminated from the v0.1.x layout:

- `crates/deliberate-mcp/` → folded into `crates/think-and-ship/src/think/` + shared engine
- `crates/resolute-mcp/` → folded into `crates/think-and-ship/src/ship/` + shared engine
- `crates/think-and-ship-core/` → folded into the new crate (project_id, sanitization, session id helpers — these were never useful on their own)
- `npm/deliberate-mcp/` and `npm/resolute-mcp/` → deprecated stubs that point at `npm/think-and-ship/`

A single `Cargo.toml` workspace. A single `cargo install think-and-ship`
gives the user the binary, the library, and the MCP server.

---

## Module layout

Inside `crates/think-and-ship/src/`:

```
src/
├── think/                 — reasoning trace domain + 11 think_* tools
│   ├── domain.rs          — Step, Branch, History, StepNumber, BranchId
│   ├── engine/            — ReasoningServer + recovery, validation, core, impact
│   ├── tools.rs           — the 11 #[tool] handlers wired through ToolFamily
│   └── descriptions.rs    — tool description strings + instructions text
├── ship/                  — execution tracking domain + 11 ship_* tools
│   ├── domain.rs          — Objective, Task, Action, Check, Artifact, TaskId, ActionId
│   ├── engine/            — ExecutionServer + recovery, validation, transitions
│   ├── tools.rs           — the 11 #[tool] handlers wired through ToolFamily
│   └── descriptions.rs    — tool description strings + instructions text
├── engine/                — shared infrastructure (no domain logic)
│   ├── project_id.rs      — basename + fnv1a_6hex identity (was think-and-ship-core)
│   ├── persistence.rs     — atomic JSON IO under a single XDG data dir
│   ├── broadcast.rs       — NDJSON-over-Unix-socket fan-out
│   ├── session.rs         — session id resolution, auto-session naming
│   └── cross_ref.rs       — the CrossRef enum + wire conversions
├── mcp/                   — MCP wire adapter (transport-agnostic)
│   ├── service.rs         — ServerHandler impl, list_tools dispatcher
│   ├── families.rs        — ToolFamily trait + registry
│   ├── schemas.rs         — outputSchema / structuredContent helpers
│   └── annotations.rs     — _meta, deprecation_warning, etc.
├── cli/                   — CLI surface (subcommands, help, init, doctor, status)
│   ├── serve.rs           — `think-and-ship serve [--http :PORT]`
│   ├── init.rs            — IDE detection + .mcp.json writing
│   ├── doctor.rs          — health checks
│   ├── status.rs          — project info dump
│   └── export.rs          — trace export passthrough
├── lib.rs                 — re-exports the public surface
└── main.rs                — subcommand dispatch
```

Two domain trees (`think/`, `ship/`) sit side by side. Neither imports
the other. They cooperate only through `engine::cross_ref::CrossRef`
and the shared persistence / broadcast layers. This is the DIP
boundary: domain types never know about wire format, persistence, or
the other family.

The `engine/` module is *infrastructure only* — it owns no domain
concepts. Both `think/` and `ship/` depend on it; it does not depend
on either.

---

## The `ToolFamily` trait

Each MCP tool family — `think_*`, `ship_*`, and any future families
(`audit_*`, `experiment_*`, …) — implements `ToolFamily`. Registration
is by composition, not by editing a central dispatcher.

```rust
pub trait ToolFamily: Send + Sync {
    /// Namespace prefix: "think", "ship", …
    fn prefix(&self) -> &'static str;

    /// All tools this family exposes, with handlers attached.
    fn tools(&self) -> Vec<ToolEntry>;

    /// Instructions text returned in the MCP `initialize` response.
    fn instructions(&self) -> &'static str;

    /// Optional: per-family deprecated aliases (e.g. "deliberate_record_step"
    /// → "think_record_step") served for one release with a deprecation_warning.
    fn deprecated_aliases(&self) -> Vec<AliasEntry> { Vec::new() }
}
```

`ToolEntry` carries the tool name, input schema, output schema, handler
function pointer, and any MCP annotations. The wire adapter in `mcp/`
walks the registered families to build the `list_tools` response and
to dispatch incoming `call_tool` requests by prefix.

**Why this shape:**

- **OCP** — adding a family adds a file. It doesn't modify `mcp/service.rs`
  or anything in another family.
- **ISP** — a client that only cares about `think_*` ignores `ship_*`
  entirely. The list_tools response is naturally namespaced.
- **Testability** — each family can be exercised in isolation with a
  fake registry, no real MCP transport needed.

The trait is **not** an inheritance hierarchy. Tool handlers do not
share a base class; they share a contract.

---

## Typed `CrossRef`

The v0.1.x cross-reference is a string (`execution_ref: "task:auth-refactor"`)
parsed at use sites. The merge replaces this with a typed enum
*in-process* while preserving the string form on the wire.

```rust
pub enum CrossRef {
    ThinkStep(StepNumber),
    ShipTask(TaskId),
    ShipAction(ActionId),
    ShipCheck(CheckId),
}

// in think::domain:
pub struct Step {
    pub refs: Vec<CrossRef>,   // any number of typed refs out
    // ...
}

// in ship::domain:
pub struct Action {
    pub thinks: Vec<StepNumber>,  // typed back-pointers in
    // ...
}
```

The MCP wire surface still accepts and emits the string form
(`"task:..."`, `"action:42"`, `"check:cargo-test"`) for backward
compatibility. `engine::cross_ref` is the single place that parses
and serializes; the rest of the codebase only ever sees the enum.

**Why this matters:**

- A `CrossRef::ShipTask("does-not-exist")` cannot accidentally be
  written as `CrossRef::ShipAction("does-not-exist")` — the type
  system enforces the kind.
- Validation runs once at the wire boundary, not on every read.
- The viewer (Tauri app) can pattern-match on the enum to render
  different link types differently, without parsing strings.

---

## Subcommand binary

The binary defaults to printing help. MCP clients invoke `serve`.

```text
think-and-ship                     # print help (default)
think-and-ship serve               # run as MCP server on stdio
think-and-ship serve --http :8080  # run as remote MCP server (Streamable HTTP)
think-and-ship init                # set up project MCP config + optional CLAUDE.md
think-and-ship init --full         # MCP config + CLAUDE.md in one shot
think-and-ship doctor              # diagnose setup issues
think-and-ship status              # show project info and config state
think-and-ship export              # export traces (markdown / json)
```

`.mcp.json` becomes one entry instead of two:

```json
{
  "mcpServers": {
    "think-and-ship": {
      "command": "think-and-ship",
      "args": ["serve"],
      "env": { "THINK_AND_SHIP_PERSIST": "true" }
    }
  }
}
```

This is the established Rust MCP pattern (one binary, multiple
subcommands; same approach as e.g. `mcp_agent_mail_rust`).

---

## Persistence and broadcast

### Persistence

One XDG data root for the whole server:

```
~/.local/share/think-and-ship/
├── think/
│   └── sessions/<project_id>-<session>.json
└── ship/
    └── sessions/<project_id>-<session>.json
```

Atomic write semantics (write to `.tmp`, fsync, rename) are preserved
from v0.1.x. Both families share the same persistence implementation
in `engine/persistence.rs`. They write to disjoint subdirectories so
the two domains stay isolated on disk; a future family adds another
subdirectory and gets persistence for free.

### Broadcast

One NDJSON-over-Unix-socket fan-out, with a `family` tag on every
frame:

```
~/.local/share/think-and-ship/broadcast.sock
```

```jsonl
{"family":"think","kind":"step_appended","step":{...}}
{"family":"ship","kind":"action_recorded","action":{...}}
{"family":"think","kind":"branch_status_changed",...}
```

The viewer connects to one socket and interleaves frames into a single
timeline. This is a meaningful simplification over v0.1.x, where the
viewer maintained two independent socket readers and merged frames
client-side.

Frames are versioned via a `schema_version` field so older viewers
can warn and refuse rather than mis-parse new shapes.

---

## Environment variables

New (canonical) names, all prefixed `THINK_AND_SHIP_`:

| Variable | Purpose | Default |
|---|---|---|
| `THINK_AND_SHIP_PERSIST` | Enable session persistence | `true` |
| `THINK_AND_SHIP_DATA_DIR` | Override the XDG data root | `~/.local/share/think-and-ship/` |
| `THINK_AND_SHIP_BROADCAST_PATH` | Unix socket path for live frames | `~/.local/share/think-and-ship/broadcast.sock` |
| `THINK_AND_SHIP_PROJECT_NAME` | Override project identity | derived from cwd |
| `THINK_AND_SHIP_AUTO_SESSION` | Enable the default-session-id fallback (uses the resolved `project_id`, stable across server spawns — no timestamp, no random suffix) | `false` |
| `THINK_AND_SHIP_DEFAULT_SESSION_ID` | Explicit session id override (wins over auto) | unset |

Old (deprecated, accepted for one release with a log warning):

- `DELIBERATE_PERSIST`, `DELIBERATE_DATA_DIR`, `DELIBERATE_BROADCAST_PATH`, `DELIBERATE_PROJECT_NAME`, `DELIBERATE_AUTO_SESSION`, `DELIBERATE_DEFAULT_SESSION_ID`
- `RESOLUTE_PERSIST`, `RESOLUTE_DATA_DIR`, `RESOLUTE_BROADCAST_PATH`, `RESOLUTE_PROJECT_NAME`

Old names map onto the new ones via a translation table at startup.
A new name always wins over an old one if both are set.

---

## Migration story

The shipped sequence (Phases 15 → 17 in the roadmap):

1. Publish `think-and-ship` v0.2.0 with the merged server.
2. Publish `deliberate-mcp` v0.3.2 and `resolute-mcp` v0.1.2 as stubs
   that print a deprecation message pointing at think-and-ship (and
   exit non-zero so misconfigured MCP clients fail loudly rather than
   silently lose data).
3. `npm deprecate deliberate-mcp` and `npm deprecate resolute-mcp` with
   messages pointing at think-and-ship.
4. `cargo yank` the v0.3.1 / v0.1.1 releases of the old crates.

### Data migration

On first startup, the server checks for v0.1.x data dirs and migrates
them in one direction:

```
~/.local/share/deliberate-mcp/sessions/*.json
    → ~/.local/share/think-and-ship/think/sessions/*.json

~/.local/share/resolute-mcp/sessions/*.json
    → ~/.local/share/think-and-ship/ship/sessions/*.json
```

Rules:

- **Idempotent**: a `.migrated-from-v0.1` marker file in the new dir
  short-circuits the check on subsequent runs.
- **One-way**: the old dirs are read-only after migration. The server
  writes only to the new dirs.
- **Safe**: if the new dirs already contain content the user has been
  writing to (e.g. an out-of-order install), the migration logs a
  warning and skips — it never clobbers.

### Tool name migration

Old tool names (`deliberate_record_step`, `resolute_set_objective`, …)
are kept as registered aliases for one release. Each alias entry has
`_meta.deprecation_warning` set per the MCP spec, so MCP clients that
surface metadata can warn the agent. The agent's existing prompts and
saved memories continue to work unchanged through the transition.

v0.3.0 drops the aliases.

---

## SOLID checklist

A `ministr_solid` audit of v0.1.x finds **zero** violations across the
workspace. The merge is designed to preserve that.

| Principle | Application |
|---|---|
| **SRP** | The server records "agent intent + execution." Each tool has one job. Each engine sub-module is concern-focused (validation, recovery, branching, revisions, lookup, snapshots). |
| **OCP** | Tool families register via the `ToolFamily` trait. Adding a third family (e.g. `audit_*`) does not modify any existing family or the wire adapter. |
| **LSP** | Shared types (`StepNumber`, `TaskId`, `ProjectId`, `SessionId`) work identically across both halves. The viewer treats them uniformly. |
| **ISP** | Clients consume by namespace prefix. A `think_*`-only consumer ignores `ship_*` and vice versa. No god-interface. |
| **DIP** | Domain types (`Step`, `Action`, `Check`, `Objective`) are pure. Persistence, broadcast, and wire format depend on the domain — never the reverse. |

Every code change should be reviewable against this table: a refactor
that breaks one of these rows is a refactor that should be rethought.

---

## MCP specification version

Targets **MCP `2025-06-18`** transitively via rmcp 1.7. Both stdio and
Streamable HTTP transports advertise this protocol version on
`initialize`.

The `2026-07-28` Release Candidate is the next breaking revision:
stateless transport (`Mcp-Session-Id` removed, `initialize` handshake
gone, `_meta`-envelope routing), Extensions framework, hardened OAuth,
JSON Schema 2020-12 default (already met via schemars 1.x), and the
`-32002` → `-32602` error-code flip for missing resources.

**Migration is gated on rmcp**, tracked at
[modelcontextprotocol/rust-sdk#526](https://github.com/modelcontextprotocol/rust-sdk/issues/526)
(SEP-1442 statelessness). The architectural commitment that makes this
safe is the **separation of the application-level stable session id
from the protocol session id**:

- The application session id (`<project_basename>-<6hex>` from
  `infra::resolve_project_id` → `infra::resolve_default_session_id`)
  keys persistence files and broadcast frames. It is process-wide,
  derived from the working directory, and orthogonal to whatever
  identity the transport assigns to a single client connection.
- The protocol session id (rmcp's `LocalSessionManager`, wired in
  `crates/think-and-ship/src/cli/mod.rs`) is owned by the transport
  layer and is what `2026-07-28` removes. We never read it.

When rmcp ships statelessness support, the migration on our side is
expected to be one wiring change: drop `LocalSessionManager`, pass
whatever stateless equivalent rmcp exposes. Application state and
documented behavior do not change.

**Deprecation window guarantee:** SEP-2596 requires ≥12 months between
a spec being deprecated and being removed, so a `2025-06-18` server
stays valid against `2026-07-28`-aware clients for at least a year
after the new spec ships. We have time to migrate cleanly.

---

## Non-goals

- **Not a project management tool.** No sprints, no Kanban, no story
  points.
- **Not a CI/CD system.** The server records check results; it does
  not run tests, lint, or builds.
- **Not opinionated about process.** The agent decides what to record.
  The server stores it.
- **Not a single monolithic binary that hides its internals.** The
  library surface is part of the public contract — the Tauri viewer
  and integration tests consume it directly.
- **Not a replacement for IDE-native AI memory.** Cross-references
  point into traces; they don't claim to be a knowledge graph.

---

## Sources

Design decisions in this document are grounded in:

- **AWS DDD on MCP boundaries** — *"Rediscovering Domain-Driven Design,
  One MCP Server at a Time"* (dev.to/aws, May 2026). Server boundaries
  should reflect agent responsibility, not tool availability.
- **MCP design guidelines** — *github.com/awslabs/mcp/blob/main/DESIGN_GUIDELINES.md*.
  Tool naming, fully-qualified-name length budgets, `-mcp` suffix convention.
- **Subcommand pattern** — *github.com/Dicklesworthstone/mcp_agent_mail_rust*.
  "Same CLI surface, one binary."
- **Multi-server orchestration costs** — getknit.dev/blog/scaling-ai-capabilities-using-multiple-mcp-servers-with-one-agent.
- **MCP spec** — modelcontextprotocol.io for `_meta.deprecation_warning`,
  outputSchema / structuredContent, and the Streamable HTTP transport.
- **Internal SOLID audit** — `ministr_solid` on the v0.1.x workspace
  (2026-05-27): 0 findings.

When a future phase contradicts any decision here, update this document
in the same commit. The roadmap is the *intent*; this doc is the
*contract*.
