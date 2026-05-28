# think-and-ship trace schema (`.think-and-ship/`)

> **Status: design spec (Phase 23a).** This documents the on-disk wire format
> for git-native shared traces. The *writer* that produces these files is
> Phase 23b — nothing in the codebase emits this format yet. This file is the
> contract 23b implements against.

## Goal

Let a team accumulate think-and-ship reasoning + execution traces **in their
git repo**, in a format that is a **strict superset of [Agent
Trace](https://agent-trace.dev/)** (the Cursor/Cognition open standard, v0.1.0,
Jan 2026). Generic Agent Trace tooling must read our records as compliant; the
richer `think_*` / `ship_*` semantics ride along in the spec's extension slot.

## What Agent Trace actually is (and isn't)

Audited from [cursor/agent-trace](https://github.com/cursor/agent-trace),
v0.1.0. Two facts shaped this design and **correct an earlier roadmap
assumption**:

1. **Agent Trace is storage-agnostic.** The spec explicitly refuses to define
   where records live: *"local files, git notes, a database, or anything
   else."* There is **no** mandated `.agent-trace/traces.jsonl`. So our choice
   of JSONL-per-session is *ours*; we don't claim the spec requires it.
2. **Agent Trace is code-attribution-centric, not reasoning-centric.** Its
   payload answers *"which lines of which file came from which model/
   conversation."* A think-and-ship reasoning step is not that shape. We do
   **not** pretend a reasoning step is an attribution record — instead each of
   our records is a *valid Agent Trace envelope* whose rich semantics live in
   `metadata` (see below).

### Agent Trace v0.1.0 record (the part we conform to)

```json
{
  "version": "0.1.0",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2026-01-25T10:00:00Z",
  "vcs":   { "type": "git", "revision": "<commit-sha>" },
  "tool":  { "name": "think-and-ship", "version": "0.3.0" },
  "files": [ /* code attribution, optional */ ],
  "metadata": { /* reverse-domain vendor extensions */ }
}
```

- `version`, `id` (UUID), `timestamp` (RFC 3339) — **required** by Agent Trace.
- `vcs` `{type, revision}` — the git revision the record was recorded against.
- `tool` `{name, version}` — always `think-and-ship` + the server version.
- `files[]` — code attribution: `{path, conversations[{contributor{type, model_id}, ranges[{start_line, end_line, content_hash?}]}]}`.
- `metadata` — vendor extensions keyed by **reverse domain**, e.g.
  `"dev.cursor"`. We use **`"dev.thinkandship"`**.

## The think-and-ship superset

Each record is a single line of JSON (JSONL). It **is** an Agent Trace record,
plus our payload under `metadata["dev.thinkandship"]`:

```jsonc
{
  "version": "0.1.0",
  "id": "f1c9…",                       // UUID per record
  "timestamp": "2026-05-28T19:30:00Z",
  "vcs":  { "type": "git", "revision": "de31806" },
  "tool": { "name": "think-and-ship", "version": "0.3.0" },
  "files": [ /* populated only for ship code actions — see below */ ],
  "metadata": {
    "dev.thinkandship": {
      "schema": "1",                   // our extension schema version
      "family": "think" | "ship",      // which half emitted it
      "kind": "step" | "objective" | "task" | "action" | "check",
      "session_id": "think-and-ship-676f38",
      "shared": true,                  // see "Local vs shared" below
      "record": { /* the verbatim think/ship domain object */ }
    }
  }
}
```

**Graceful degradation.** A generic Agent Trace consumer reads
`version`/`id`/`timestamp`/`tool`/`vcs` and any `files[]` attribution, and
ignores the `dev.thinkandship` key it doesn't understand. A think-and-ship-aware
consumer (the viewer, analytics, `think-and-ship export`) reads the full record
from `metadata["dev.thinkandship"].record`.

### `think` step record

A reasoning step carries **empty `files[]`** (it attributes no code) and the
full `DeliberateStep` under the extension:

```jsonc
{
  "version": "0.1.0",
  "id": "…", "timestamp": "2026-05-28T19:30:00Z",
  "vcs": { "type": "git", "revision": "de31806" },
  "tool": { "name": "think-and-ship", "version": "0.3.0" },
  "files": [],
  "metadata": { "dev.thinkandship": {
    "schema": "1", "family": "think", "kind": "step",
    "session_id": "think-and-ship-676f38", "shared": true,
    "record": {
      "step_number": 64, "estimated_total": 66,
      "purpose": "Open Phase 20.5 …", "thought": "…", "outcome": "…",
      "rationale": "…", "confidence": 0.9, "pinned": true,
      "dependencies": [{ "step": 59, "relation": "supports" }],
      "execution_ref": "task:explore", "timestamp": "2026-05-28T19:14:…Z"
    }
  } }
}
```

> `record` is the existing `DeliberateStep` serialization — no field renames.
> 23b serializes the domain object verbatim into `record`.

### `ship` action record — where the superset genuinely overlaps Agent Trace

A `ship_record` action of type `code` with `files_touched` is *exactly* what
Agent Trace's `files[]` is for. We populate **both**: the Agent Trace
attribution **and** the rich extension.

```jsonc
{
  "version": "0.1.0",
  "id": "…", "timestamp": "2026-05-28T19:16:11Z",
  "vcs": { "type": "git", "revision": "de31806" },
  "tool": { "name": "think-and-ship", "version": "0.3.0" },
  "files": [
    {
      "path": "Cargo.toml",
      "conversations": [{
        "contributor": { "type": "ai", "model_id": "anthropic/claude-opus-4-8" },
        "ranges": [{ "start_line": 1, "end_line": 1 }]    // whole-file when line info is unknown
      }]
    }
  ],
  "metadata": { "dev.thinkandship": {
    "schema": "1", "family": "ship", "kind": "action",
    "session_id": "think-and-ship-676f38", "shared": true,
    "record": {
      "id": 2, "task_id": "implement", "type": "code",
      "description": "Root Cargo.toml: pruned [workspace.dependencies] …",
      "files_touched": ["Cargo.toml", "crates/deliberate-mcp/Cargo.toml"],
      "tools_used": ["Edit"], "result": "", "deliberate_step": 64,
      "timestamp": "2026-05-28T19:16:11Z"
    }
  } }
}
```

**Mapping rule (23b):** for a ship action whose `action_type` is `code`/
`refactor`, emit one `files[]` entry per `files_touched` path, with
`contributor.type = "ai"` and `model_id` resolved from the agent's model
identity (models.dev `provider/model-name` convention). Line ranges:
whole-file (`start_line: 1, end_line: <eof or 1>`) when precise ranges aren't
known — Agent Trace permits whole-file attribution. Non-code action types
(`test`/`research`/`review`/…) emit empty `files[]`.

### `objective` / `task` / `check` records

Same envelope; `kind` set accordingly; `files[]` empty; the domain object
(`Objective`, `Task`, `Check`) under `record`. A `check` of type `test`/`build`
is a quality gate, not code authorship → no `files[]`.

## On-disk layout

```
<repo-root>/.think-and-ship/
├── sessions/            # SHARED — committed to git
│   ├── think-and-ship-676f38--2026-05-28.jsonl
│   └── …                # one file per session; one record per line
└── local/               # PRIVATE — gitignored
    └── …                # same format; never committed
```

- **One file per session, one record (line) per mutation.** Appended on each
  `think_record_step` / `ship_record` / `ship_check` / etc.
- File name: `<session_id>--<date>.jsonl` (date = session start, UTC). Session
  id already namespaces by project (`<basename>-<6hex>`), so two projects never
  collide.
- The repo's `.gitignore` must list `.think-and-ship/local/` (23b adds this on
  first write; documented for teams adopting manually).

## Local vs shared — the `shared` field

Every record carries `metadata["dev.thinkandship"].shared: bool`.

| `shared` | Written to | Committed? | Meaning |
|----------|-----------|-----------|---------|
| `false` *(default)* | `.think-and-ship/local/` | No (gitignored) | Personal scratch reasoning — yours alone |
| `true` | `.think-and-ship/sessions/` | Yes | Team-visible AI Decision Record |

- **Default is `false`** — opt **in** to sharing, never opt out. (Matches the
  Cognee "default to isolation" model; the safe default for traces that may
  contain pasted secrets.)
- Promotion (`local` → `sessions`, flipping `shared` to `true`) is a deliberate
  curation step — the `think-and-ship promote` CLI in **Phase 23c**.
- A single session can contain a mix: most steps `local`, a few promoted to
  `shared`. The writer partitions line-by-line by the `shared` flag, so one
  session id can have a file in **both** `local/` and `sessions/`.

## Versioning

- `version: "0.1.0"` tracks the **Agent Trace** spec version we conform to.
- `metadata["dev.thinkandship"].schema: "1"` tracks **our extension** schema
  independently, so we can evolve our payload without implying an Agent Trace
  spec bump.

## What this spec deliberately does NOT cover (later phases)

- The writer / `SyncTarget::RepoGit` and per-session-commit mechanics → **23b**.
- Pre-commit secret redaction + the `promote` CLI → **23c**.
- Pluggable backends (Automerge / Iroh) → **Phase 25**.

## Sources

- Agent Trace spec — <https://github.com/cursor/agent-trace>, <https://agent-trace.dev/>
- "Capturing the Context Graph of Code" — <https://cognition.ai/blog/agent-trace>
- Extension-via-reverse-domain `metadata` convention — Agent Trace v0.1.0 spec.
