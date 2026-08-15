# Unified record wire contract (Phase 31a)

> **Reference** — facts to look up, not a reading path ([all docs](README.md)).

> **Status: design spec (Phase 31a).** This is the backend-agnostic contract
> the rest of the Phase-31 SaaS implements against — the multi-family backend
> (31b), the full-surface remote-MCP server (31c), the local
> `SyncTarget::Cloud` client (31d), and everything downstream. **Nothing in the
> codebase emits or consumes this format yet.** The canonical machine-readable
> schema is
> [`contract/unified-record-envelope.schema.json`](../contract/unified-record-envelope.schema.json);
> this file is its prose contract. No implementation code ships in this chunk.

## Goal

The Phase-31 thesis (see the `/roadmap-refresh` interview) is one open-core SaaS
whose product **and moat** is the unified
**think ↔ ship ↔ roadmap ↔ signal** cross-reference graph: a team's agents all
read and write one shared, queryable graph of *what was reasoned* (think),
*what was done* (ship), *what is planned* (roadmap), and *what stakeholders
asked for* (signal). Phase 30a already shipped a per-tenant cloud contract for
**one** of those families (signals). This chunk **generalizes it** so all four
flow through one store without each layer re-inventing the shape. There must be
**one** record wire form that:

1. **carries every family** (think step, ship objective/task/action/check,
   roadmap chunk, signal) behind one envelope, so the backend stores and the
   remote-MCP surface speaks one shape;
2. **makes the cross-ref graph first-class and queryable** — every family's
   links normalized into one `edges[]` list — because the graph is the moat;
3. **carries a stable idempotency key per family**, so re-sync (owner records)
   or re-delivery (ingested signals) of the same record is a no-op upsert;
4. **is a faithful generalization of the 30a Signal envelope** (no signal
   information is lost; the cloud fields are reused verbatim), so the signal
   cloud keeps working as the `family: "signal"` profile;
5. **is shared by the Rust core and the TypeScript edge from one definition**,
   so the two can never drift.

This document fixes (1)–(4) and the identity decisions below;
`unified-record-envelope.schema.json` is the artifact that delivers (5).

## Why a JSON Schema is the source of truth

Same rule as 30a. The backend splits into a **Rust core** (local
`think-and-ship`) and a **TypeScript edge** (Cloudflare Workers, 31b) that must
agree on the envelope byte for byte. Rather than hand-maintain two type
definitions, a single language-neutral **JSON Schema (draft 2020-12)** is
canonical and both ends derive from it:

| Side | How it consumes the canonical schema |
|------|--------------------------------------|
| **Rust core** | `serde`-deserialize into a hand-written `UnifiedRecordEnvelope` struct **plus** a test validating a fixture against `contract/unified-record-envelope.schema.json` — drift fails CI. |
| **TS edge** | Generate `UnifiedRecordEnvelope.ts` from the schema with `json-schema-to-typescript` at build time. No hand-written types. |

The **envelope wrapper and the `edges[]` graph are strict**
(`additionalProperties: false`): an unknown field there is a contract
violation. The **`record` payload is intentionally open** — see below.

## The envelope

A `UnifiedRecordEnvelope` is a thin, strict **wrapper** around a verbatim local
family **record**, plus the normalized graph. Field-by-field reference lives in
the schema; the shape:

```jsonc
{
  // --- identity + provenance (strict wrapper) ---
  "schema_version": "1",                       // this envelope schema's version
  "tenant_id": "think-and-ship-676f38",        // == resolve_project_id(); one tenant = one Durable Object
  "family": "think",                           // think | ship | roadmap | signal
  "kind": "step",                              // constrained to the family (see below)
  "id": "884",                                 // stable id within (tenant, family, kind)
  "idempotency_key": "a1b2…ff00",              // sha256 hex; dedupe re-sync/re-delivery
  "created": "2026-06-08T03:14:30Z",           // RFC 3339 UTC
  "updated": "2026-06-08T03:20:00Z",           // optional; set by the store on each write
  "source": { "adapter": "local", "received_at": "…" },  // optional; mainly ingested signals
  "attribution": { "authenticated": true, "subject": "…" }, // optional
  "lenses": ["think-and-ship"],                // optional (31a-ii); soft M:N membership — see "Lens"

  // --- the moat: normalized cross-ref graph ---
  "edges": [
    { "ref": "think:883", "relation": "depends_on" },
    { "ref": "task:schema-31a", "relation": "motivates" }
  ],

  // --- the verbatim local family object (open payload) ---
  "record": { "step_number": 884, "purpose": "…", "execution_ref": "task:schema-31a", … }
}
```

### `family` → `kind`

`kind` is constrained to its `family` (enforced by the schema's top-level
`allOf` / `if`-`then`):

| `family`  | allowed `kind`(s)                          | `record` is…                |
|-----------|--------------------------------------------|-----------------------------|
| `think`   | `step`                                     | a `ThinkStep`          |
| `ship`    | `objective` \| `task` \| `action` \| `check` | the matching ship object  |
| `roadmap` | `chunk`                                    | a roadmap `Chunk`           |
| `signal`  | `signal`                                   | a `Signal` (30a `record`)   |

### Why the `record` payload is open

The wrapper and edges are the **new** contract this chunk owns, so they are
strict. The `record` is the **existing** local domain object — owned by the
Rust types (`crates/think-and-ship/src/*/domain.rs`) and, for signals, fully
specified by `contract/signal-envelope.schema.json`. Re-specifying all four
families' fields here would duplicate those types and create a second drift
surface. So `record` is `type: object` with `additionalProperties` open: the
envelope guarantees *identity, provenance, and the graph*; the payload remains
whatever the local record is. Its `id` MUST equal the envelope `id`.

## The edge graph (the moat)

`edges[]` is the canonical, queryable projection of the cross-ref graph. Each
edge's **source** is the enclosing record's `<family-prefix>:<id>` endpoint;
`ref` is the **target** in the existing `prefix:value` wire form
(`think:N` | `task:X` | `action:N` | `check:X` | `chunk:X` | `signal:X`,
mirroring `infra/cross_ref.rs`). The backend indexes `(source, ref, relation)`
into one graph and serves the explorer (31g) and benchmark (31i) from it —
without parsing four different local link representations.

`edges[]` is **derivable from `record`** but **materialized** for query. How
each family's local links map in:

| Family    | Local link source                              | Becomes edges…                                  |
|-----------|------------------------------------------------|-------------------------------------------------|
| roadmap   | `Chunk.cross_refs[]` (`prefix:value` strings)  | one edge per ref (relation absent)              |
| signal    | `Signal.cross_refs[]`                          | one edge per ref                                |
| think     | `ThinkStep.dependencies[]` `{step,relation}` | `think:<step>` edges carrying the relation   |
| think     | `ThinkStep.execution_ref` (`task:…`)      | one edge                                         |
| ship      | action `think_step: N`                    | one `think:<N>` edge                            |

Optional `relation` extends the think dependency vocabulary
(`supports`/`refutes`/`depends_on`) with the cross-family names the graph
already expresses: `realizes` (chunk→task), `motivates` (think→task),
`promoted_from`/`promotes` (signal↔chunk). Absent = an unlabeled link.

## Idempotency

The key collapses re-delivery (ingested) or re-sync (owner-authored) of the
same record onto one upsert. Lowercase hex SHA-256; `␟` (U+241F) is the field
separator; computed identically on both ends. The rule is **per family**:

- **think / ship / roadmap** (owner-authored, not ingested):
  `sha256(tenant_id ␟ family ␟ kind ␟ id)`. Re-syncing the same step/chunk/task
  is a no-op; the record's own id is the stable identity.
- **signal** (may be ingested from many adapters): unchanged from 30a —
  `sha256(tenant_id ␟ source.adapter ␟ source.external_id)` when the adapter has
  a stable upstream id, else `sha256(tenant_id ␟ kind ␟ from ␟ body ␟ created)`.

The canonical store treats a colliding key as a no-op upsert (last-writer
fields win on a genuine update, distinguished by `updated`).

## Workspace identity & isolation (31a-ii)

`tenant_id` is the **Workspace** key — the one hard wall. The wire name stays
`tenant_id` (schema_version stays `"1"`, zero migration); the *vocabulary* is
Workspace at every human surface: **one workspace = one Durable Object**, and
auth, billing, sync, and isolation all bind to it. Cross-workspace reads are
never permitted — every record, edge, and query is scoped to one `tenant_id`,
and the graph never spans workspaces.

Two id shapes share the namespace today:

- `org_…` — a WorkOS organization: a true multi-member workspace (Bearer/web
  tokens carry it).
- `<basename>-<6hex(cwd)>` — a `resolve_project_id()` slug: a de-facto
  **single-project workspace**, produced by local agents and inbound-email
  ingest before any org mapping exists.

A project slug **merges into** an org workspace as a *Lens*, never as a second
wall: the **alias registry** (signal-tenant-aliasing; one reserved directory DO,
`alias.ts`) maps an inbound identity (email local-part, project slug) to
`(workspace, lens)`; ingest then writes to the org workspace's DO with
`lenses: ["<slug>"]`. Until a mapping exists, a slug workspace is valid and
standalone — unmerged, not wrong.

THE one tenant-identity story, per ingest path:

| Path | Workspace (`tenant_id`) comes from |
|---|---|
| Bearer / web / sync / MCP | the verified token (WorkOS org or minted agent token) — the edge overwrites `x-tenant-id` |
| Inbound email | recipient local-part → **alias registry**: registered ⇒ the owning org workspace + `lenses:[lens]`; unregistered ⇒ the local-part as a slug workspace (pre-aliasing behavior) |
| Submit / webhook | the signed submit token / claimed-tenant HMAC, as before — slug or org, whatever was minted |

Aliases are managed by the authed `POST /v1/aliases` (`{alias, lens?}`,
first-come per alias, 409 on cross-workspace conflict) and `GET /v1/aliases`.

## Lens — soft M:N grouping within a workspace (31a-ii)

A **Lens** is a named, many-to-many grouping over the records of one workspace
("a project is just a lens" — decided in data-project-dimension, think:1107). On the wire it is the
optional `lenses` wrapper field: an array of slug-safe strings naming the
lenses this record belongs to. Properties the model guarantees:

- **Soft, never a partition.** Lenses scope *views*; they do not shard
  storage, auth, or the graph. `neighbors()` traverses the whole workspace —
  cross-lens edges are ordinary edges.
- **Idempotency-blind.** `idempotency_key` never reads `lenses`: the same
  record in more (or different) lenses is the *same record*, not a duplicate.
- **Absent = unlensed.** Every pre-31a-ii record omits the field and stays
  byte-for-byte valid; readers surface unlensed records in the workspace's
  "All" view. For a slug workspace the implicit sole lens *is* the slug — when
  such a workspace merges into an org, that slug seeds the records' initial
  lens (the cwd id seeds an initial lens).
- **Membership only.** Lens display metadata (name, shape) is not a wire
  record in this phase: the SPA keeps it client-first, and the aliasing
  registry is its future server-side home. A lens CRUD surface is deliberately
  deferred until a consumer exists.

## Approval gates (`family: ship`, `kind: gate`) — webapp-approval-gates

Agent work that must pause for a human yes is a **record**, not a protocol
side-channel: the pause, the pending decision, the answer, and the outcome are
all one `ship`/`gate` envelope crossing the same tenant boundary as every
other record. The design was verified against the engine's actual behavior
(the elicitation seam's headless discipline) rather than assumed.

The gate `record`:

```json
{
  "id": "<uuid>",
  "question": "one plain-language sentence a human answers",
  "body": "plain-prose context: what happens on each answer, what was verified",
  "options": [ { "key": "deploy", "label": "Deploy now" }, { "key": "hold", "label": "Hold for review" } ],
  "default_key": "hold",
  "state": "pending | answered | expired",
  "opened_at": "RFC 3339",
  "expires_at": "RFC 3339",
  "answer": { "choice": "deploy", "note": "optional", "decided_by": "…", "decided_at": "RFC 3339" }
}
```

The protocol, and who writes what:

- **The engine opens** (`ship_gate_open`): builds the record with a **required**
  `default_key` and a **required** `expires_at` (timeout clamped 30 s–7 d), and
  `PUT`s it like any envelope. The gate's id is its own UUID — *not* the
  cycle-scoped id the other ship kinds use — because a gate must stay
  addressable across `ship_reset` while an agent is still waiting on it. With
  no cloud workspace connected the open call **resolves to the default
  immediately, in words** — a gate that nobody could see must never hang a
  headless session (the same law `mcp/elicit.rs` encodes for local
  elicitation).
- **The browser answers** (`POST /v1/gate-answer`, edge-authenticated): the
  route is the ONE writer of `answer`. It validates the choice against
  `options`, refuses a second answer (409 — first answer wins), refuses an
  answer at/past `expires_at` (410 — the default already applies), and stamps
  `decided_by`/`decided_at` from the edge-verified `x-user-id`/`x-user-email`
  headers — a body-supplied identity can never claim the decision. Agent
  tokens carry no `x-user-id` and are refused (403): a gate exists precisely
  so a *human* decides. The write broadcasts the updated envelope on
  `/v1/events` like any record change.
- **The engine resumes** (`ship_gate_wait`): a **bounded** poll (≤ 55 s per
  call, under any MCP client timeout; the agent loops) of
  `GET /v1/records/ship/gate/{id}`, resolved by ONE pure rule every reader
  shares: an `answer` wins over everything; else the gate is `expired` once
  `now >= expires_at` (resolved choice = `default_key`); else `pending`.
- **`expired` is derived, never written.** No writer materializes it: the
  engine writes the gate at open, the answer route writes answers, and the
  clock decides expiry — so an unanswered gate's outcome needs no write to be
  known, and readers with skewed clocks cannot disagree (a stored `answer`
  beats a locally-computed expiry; the route's refusal past `expires_at` is
  what makes that safe).

Exercised by: `crates/…/src/ship/gate.rs` unit tests +
`ship_gate_envelope_validates` (Rust, against this schema),
`backend/test/gate-answer.test.ts` (route invariants), and the frontend gate
model tests.

## Relationship to the 30a Signal envelope

The unified envelope is a **faithful generalization**, not a replacement:

- The cloud fields `schema_version`, `tenant_id`, `idempotency_key`, `source`,
  `attribution` are **reused verbatim** (the `Source` and `Attribution` `$defs`
  are byte-for-byte the 30a definitions).
- A 30a `SignalEnvelope` carries the local `Signal` fields **flattened** at the
  top level; the unified form carries the same `Signal` **nested** under
  `record` (`family: "signal"`, `kind: "signal"`), with the signal's
  `cross_refs[]` also projected into `edges[]`. No signal information is lost —
  the mapping is a mechanical lift (cloud fields up, local fields into `record`)
  and is reversible.
- `contract/signal-envelope.schema.json` remains the **authoritative shape of
  the signal `record`** (and the wire form for the signal-only ingest path);
  the unified envelope governs how *any* record, including a signal, crosses the
  tenant boundary in the multi-family backend.

## What this chunk does NOT do

- No emit/consume code. The Rust `UnifiedRecordEnvelope` struct + drift test
  land with 31b/31d; the TS type generation lands with 31b.
- No backend, no auth server, no sync. Those are 31b (backend), 31c
  (remote-MCP + OAuth 2.1), 31d (sync client).
- No re-specification of the four local domain objects — `record` stays open and
  the local Rust types remain their source of truth.

## Validation

`unified-record-envelope.schema.json` is checked under **ajv strict mode**:
the schema compiles strict, one worked **example per family** (think / ship /
roadmap / signal) round-trips, and negative controls are rejected (unknown
wrapper field, bad `family`, `family`/`kind` mismatch, an edge `ref` missing its
prefix, a bad `relation`, a malformed `idempotency_key`, a missing `record`).

## Sources

- `contract/signal-envelope.schema.json` + `docs/SIGNAL_CONTRACT.md` — the 30a
  contract this generalizes.
- `crates/think-and-ship/src/infra/cross_ref.rs` — the `prefix:value` wire form
  the `edges[].ref` and `CrossRef` `$def` mirror.
- The `/roadmap-refresh` interview (think steps 883–885) — the Phase-31 thesis
  (unified graph as front door + moat) this contract serves.
