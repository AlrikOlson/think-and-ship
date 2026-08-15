# Signal wire contract (Phase 30a)

> **Reference** — facts to look up, not a reading path ([all docs](README.md)).

> **Status: design spec (Phase 30a).** This is the backend-agnostic contract
> every other Phase-30 chunk implements against — the cloud backend (30b), the
> edge ingest adapters (30c), the remote-MCP surface (30d), and the local
> `SyncTarget::Cloud` client (30e). **Nothing in the codebase emits or consumes
> this format yet.** The canonical machine-readable schema is
> [`contract/signal-envelope.schema.json`](../contract/signal-envelope.schema.json);
> this file is its prose contract. No implementation code ships in this chunk.

## Goal

Let stakeholder **signals** (questions, ideas, concerns, bugs, feedback) flow
from many sources — a collaborator's GitHub issue, an email, a web form, a
collaborator's own agent — into one **per-tenant, system-of-record** store, and
back out to the project owner's local `think-and-ship` as a cache. For that to
work without every layer re-inventing the shape, there must be **one** signal
wire form that:

1. **every ingest adapter normalizes to** (30c), so the store is source-agnostic;
2. **carries a stable idempotency key**, so re-delivery of the same upstream
   event is a no-op (webhook/at-least-once delivery is a known 2026 pain point);
3. **is a strict superset of the local `Signal` type** (Phase 29a), so a synced
   signal round-trips into the local cache with no lossy mapping;
4. **is shared by the Rust core and the TypeScript edge from one definition**,
   so the two can never drift.

This document fixes (1)–(3) and the identity/auth/sync decisions below;
`signal-envelope.schema.json` is the artifact that delivers (4).

## Why a JSON Schema is the source of truth

The backend splits the repo: the **Rust core** (local `think-and-ship`) and a
**TypeScript edge** (Cloudflare Workers, 30b) must agree on the envelope byte
for byte. Rather than hand-maintain two type definitions, we make a single
language-neutral **JSON Schema (draft 2020-12)** canonical and derive both ends
from it:

| Side | How it consumes the canonical schema |
|------|--------------------------------------|
| **Rust core** | `serde`-deserialize into a hand-written `SignalEnvelope` struct **plus** a test that validates the struct's `schemars`-emitted schema (or a fixture) against `contract/signal-envelope.schema.json` — drift fails CI. |
| **TS edge** | Generate `SignalEnvelope.ts` from the schema with `json-schema-to-typescript` at build time. No hand-written types. |

The schema is **strict** (`additionalProperties: false` at every level): an
unknown field is a contract violation, not silently dropped. This is the direct
mechanism behind acceptance criterion "Rust core and TS edge share the envelope
definition (no schema drift)."

## The Signal envelope

A `SignalEnvelope` is the local `Signal` (Phase 29a) plus the cloud fields every
adapter populates. Field-by-field reference lives in the schema; the shape:

```jsonc
{
  // --- cloud envelope fields ---
  "schema_version": "1",                         // this envelope schema's version
  "tenant_id": "think-and-ship-676f38",          // == resolve_project_id(); one tenant = one Durable Object
  "idempotency_key": "9f2c…8f",                  // sha256 hex; dedupe re-delivery (see below)
  "source": {                                     // which edge adapter produced it (30c)
    "adapter": "github_issue",                    // webhook | github_issue | email | submit_api | mcp | local
    "external_id": "I_kwDOABCDEF…",               // adapter's stable upstream id, when any → feeds the key
    "received_at": "2026-06-08T01:10:03Z"
  },
  "attribution": {                                // structured, optionally-authenticated identity
    "display_name": "Dana Okoye", "handle": "danao",
    "authenticated": true, "subject": "auth0|653f…"   // OAuth 2.1 access-token sub when proven
  },

  // --- the local Signal (Phase 29a), unchanged ---
  "id": "f1c9a4e2-…",                            // UUIDv4, identifies the RECORD
  "kind": "feedback",                            // question | idea | concern | bug | feedback
  "from": "dana@acme.example",                   // cheap always-present display label
  "body": "The roadmap export drops sub-bullets…",
  "status": "new",                               // new→triaged→researched→surfaced→promoted | dismissed
  "created": "2026-06-08T01:10:00Z",
  "updated": "2026-06-08T01:12:00Z",             // set by the store on every write
  "enrichment": [ /* append-only agent trail, Phase 29d */ ],
  "cross_refs": ["think:867", "chunk:richer-roadmap-export"]   // prefix:value, incl. signal:
}
```

**`id` vs `idempotency_key`.** `id` identifies the *record* (a fresh UUID per
signal). `idempotency_key` identifies the *upstream event*, so two deliveries of
the same GitHub issue collapse onto one record. They are deliberately separate.

### The idempotency-key rule

Every adapter computes the key **identically** so the store can treat a
collision as a no-op upsert. Lowercase hex SHA-256 of fields joined by the
field-separator control character `␟` (U+241F):

- **When the adapter has a stable upstream id** (`source.external_id` present):

  ```
  idempotency_key = sha256( tenant_id ␟ source.adapter ␟ source.external_id )
  ```

- **Otherwise** (anonymous webhook, form post with no upstream id):

  ```
  idempotency_key = sha256( tenant_id ␟ kind ␟ from ␟ body ␟ created )
  ```

`tenant_id` is always in the preimage, so a key can never collide across
tenants. The store rejects a write whose `idempotency_key` doesn't recompute
from the envelope (tamper / drift guard).

## Tenant identity

- **Tenant key = `resolve_project_id()`.** The cloud `tenant_id` is exactly the
  value the local server already derives — `<basename>-<6hex(cwd)>`, or the
  `THINK_AND_SHIP_PROJECT_NAME` override (`infra/project_id.rs`). So the same
  working directory maps deterministically to the same cloud tenant, and
  `think_*`/`ship_*`/`roadmap_*`/`signal_*` all share one identity.
- **One tenant = one Durable Object (30b).** Each tenant's signals live in a
  single DO instance backed by D1. This is the structural answer to "multi-tenant
  + AI agents is the deadliest risk": isolation is enforced by *which object you
  can address*, not by a `WHERE tenant_id = ?` clause that a bug could omit.
- **Tenant isolation guarantee.** A request authenticated for tenant A can never
  read or write tenant B. The auth layer resolves the token → exactly one
  `tenant_id` → exactly one DO; there is no cross-tenant query surface. 30b's
  acceptance includes a test that proves A cannot read B.

## Auth — OAuth 2.1 resource server

**Decision (Phase 30a, user-confirmed): the backend is an OAuth 2.1 resource
server now**, not a bespoke long-lived API token. This is the standards-aligned
path the current MCP authorization spec (2025-06-18) mandates for a protected
MCP server, and it gives per-scope, short-lived credentials from day one.

- The signals backend **validates** access tokens; it does **not** issue them.
  Tokens come from an external authorization server / IdP (the resource-server
  pattern — RFC 9728 protected-resource metadata, OAuth 2.1).
- An unauthenticated request gets `401` with a `WWW-Authenticate` header
  pointing at the protected-resource metadata, per the MCP auth spec.
- **Scopes** gate the signal operations:
  - `signal:read` — list/get signals for the authenticated tenant;
  - `signal:write` — capture/enrich/transition;
  - `signal:promote` — promote a signal to a roadmap chunk.
- The validated token's `sub` is recorded as `attribution.subject` with
  `attribution.authenticated: true`. Anonymous public-submit (30c/30f) produces
  a signal with `authenticated: false` and no `subject`.
- Both **the owner's local agent and collaborators' own agents** authenticate
  the same way against the same surface — that is the point of going remote-MCP.

> **Forward note for 30b/30d.** v1 may bootstrap with a single first-party
> authorization server (or a hosted IdP) issuing the three scopes; the contract
> only fixes that the backend is a *resource server* validating bearer tokens
> with these scopes. The token format (JWT vs opaque + introspection) is a 30b
> implementation choice, invisible to this envelope.

## Sync — remote-MCP over streamable HTTP

**Decision (Phase 30a, user-confirmed): the backend is exposed AS a remote MCP
server over streamable HTTP** (the 2025-03-26 transport; SSE is deprecated), not
a bespoke REST client.

- The same `signal_*` tool surface serves the owner's local agent **and**
  collaborators' agents — list, get, capture, enrich, promote — against the
  canonical store. No second client to build or document.
- **Auth rides the transport**: the streamable-HTTP endpoint is the OAuth 2.1
  resource server above; the bearer token selects the tenant.
- **Real-time push (30d)**: a tenant Durable Object pushes new-signal
  notifications to connected agents (WebSocket hibernation / SSE), so a connected
  agent learns of a new signal live instead of polling. Connection drop + resume
  must not lose or duplicate signals (the `id` + `idempotency_key` pair makes
  reconciliation deterministic).
- The local `SyncTarget::Cloud` client (30e) is a *client* of this surface:
  pull tenant signals into the local cache, reconcile (**cloud wins** on
  conflict — local is a cache, not a fork), write promotions/lifecycle changes
  back, fall back to poll when push is unavailable.

**REST fallback (recorded, not chosen).** A plain REST+JSON surface
(`GET/POST /v1/tenants/{t}/signals`, bearer-token auth, poll for realtime) was
considered. It is simpler at the edge but requires a one-off Rust client and
gives collaborators' agents no MCP-native access; realtime degrades to polling.
Rejected for v1 in favor of remote-MCP. If remote-MCP proves operationally
heavy, the envelope and idempotency rule are transport-independent and would
carry over to a REST surface unchanged.

## `signal:` cross-ref (forward note for 29b)

`cross_refs[]` uses the existing `prefix:value` wire form
(`infra/cross_ref.rs::CrossRef`). Phase 29b adds the `signal:<id>` variant
alongside `think:N | task:X | action:N | check:X | chunk:X`. Promotion is
bidirectional: `signal_promote` writes `chunk:<id>` onto the signal and
`signal:<id>` onto the new chunk, so provenance runs both ways — a roadmap chunk
can always name the stakeholder signal that motivated it.

## Versioning

- `schema_version: "1"` tracks **this envelope schema** independently of the
  Agent-Trace `version` (docs/SCHEMA.md), the `dev.thinkandship` extension
  `schema`, the backend API version, and the local-Signal version. A breaking
  envelope change bumps only this.
- The canonical schema's `$id` carries the version
  (`…/signal-envelope/v1.json`); a v2 lives at a new `$id`, and the store may
  serve both during a migration window.

## What this spec deliberately does NOT cover (later phases)

- The Cloudflare backend — Workers + per-tenant Durable Object + D1 store, CRUD,
  lifecycle enforcement, tenant provisioning → **30b**.
- The edge ingest adapters (webhook / GitHub Issues / Cloudflare Email Service /
  submit API) that normalize to this envelope → **30c**.
- The remote-MCP server transport + real-time push mechanics → **30d**.
- The local `SyncTarget::Cloud` client (cache, reconcile, write-back) → **30e**.
- The collaborator web UI (submit portal + triage dashboard) → **30f**.
- The local `signal_*` family + local `Signal` type → **29a** (the subset this
  envelope supersets).

## Sources

- Model Context Protocol — Authorization (OAuth 2.1 resource server, streamable
  HTTP) — <https://modelcontextprotocol.io/specification/draft/basic/authorization>
- "Authentication and authorization in Model Context Protocol", Stack Overflow
  blog, Jan 2026 — streamable HTTP + OAuth 2.1, SSE deprecated.
- RFC 9728 — OAuth 2.0 Protected Resource Metadata.
- JSON Schema draft 2020-12 — <https://json-schema.org/draft/2020-12>
- Existing substrate: `infra/project_id.rs` (tenant key), `infra/cross_ref.rs`
  (cross-ref wire form), `infra/repo_sync.rs` (`SyncTarget`), `docs/SCHEMA.md`
  (the house contract-doc precedent this mirrors).
