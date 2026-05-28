# think-and-ship

[![CI](https://github.com/AlrikOlson/think-and-ship/actions/workflows/ci.yml/badge.svg)](https://github.com/AlrikOlson/think-and-ship/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/think-and-ship)](https://www.npmjs.com/package/think-and-ship)
[![crates.io](https://img.shields.io/crates/v/think-and-ship)](https://crates.io/crates/think-and-ship)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

One MCP server. Two tool families. **`think_*`** records *why* the agent
is doing something (reasoning trace: steps, branches, revisions,
confidence). **`ship_*`** records *what* the agent did (execution trace:
objectives, tasks, actions, quality gates, artifacts). Cross-references
between the two families auto-correlate by project identity, giving you
a full audit trail from "what the agent thought" to "what shipped."

> **v0.2.0 — the merge.** `deliberate-mcp` and `resolute-mcp` (the
> v0.1.x duo) are now one unified server. The old tool names work as
> deprecated aliases through v0.2.x. See
> [CHANGELOG.md](CHANGELOG.md#020--2026-05-27) for the migration.

## Quickstart

```sh
npm install -g think-and-ship
cd your-project
think-and-ship init --full
```

Done. Binary installed, MCP config written for your IDE, CLAUDE.md
generated with a tool reference. Open a conversation and go.

Auto-detects **Claude Code**, **Cursor**, and **Windsurf**.

## What you get

22 canonical tools across two families, plus 22 deprecated aliases
(`deliberate_*` / `resolute_*`) wired with `_meta.deprecation_warning`
per the MCP spec so v0.1.x prompts keep working.

### `think_*` — the thinking track (11 tools)

Records structured reasoning. The agent writes down *why* before it acts:
steps, branches, revisions, confidence, dependencies, pinned conclusions.

```
think_record_step → think_pin_step → think_trace_checkpoint
```

[Full tool reference below.](#think-tools)

### `ship_*` — the doing track (11 tools)

Records structured execution: objectives, task plans, actions, quality
gates, artifacts. The agent tracks *what* it did, *whether it passed*,
and *what it shipped*.

```
ship_set_objective → ship_plan → ship_start → ship_record → ship_check → ship_finalize
```

[Full tool reference below.](#ship-tools)

### Cross-references

The two families link to each other automatically:

```
think_record_step:
  execution_ref: "task:auth-refactor"   # points at a ship_* task

ship_record:
  deliberate_step: 19                   # points at think_* step #19
```

Both halves resolve the same `project_id` from your working directory,
so traces from different conversations in the same project correlate.

## Install

```sh
# npm (recommended — downloads prebuilt binaries, no Rust needed)
npm install -g think-and-ship

# cargo (from crates.io)
cargo install think-and-ship

# verify
think-and-ship --check
```

## Configure

`think-and-ship init` auto-detects your IDE and writes the config:

| IDE         | Config file        | Detection                  |
|-------------|--------------------|----------------------------|
| Claude Code | `.mcp.json`        | default                    |
| Cursor      | `.cursor/mcp.json` | `.cursor/` dir exists      |
| Windsurf    | `.windsurf/mcp.json` | `.windsurf/` dir exists  |

The generated config — **one entry**, not two:

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

## CLI

| Command                              | What it does                                  |
|--------------------------------------|-----------------------------------------------|
| `think-and-ship serve`               | Run as MCP server on stdio                    |
| `think-and-ship serve --http :8080`  | Run as MCP server over Streamable HTTP        |
| `think-and-ship init`                | Write MCP config for your IDE                 |
| `think-and-ship init --full`         | MCP config + CLAUDE.md tool reference         |
| `think-and-ship init --dry-run`      | Preview without writing                       |
| `think-and-ship init --force`        | Overwrite existing config                     |
| `think-and-ship doctor`              | Diagnose setup issues                         |
| `think-and-ship status`              | Project info + config state                   |
| `think-and-ship --check`             | Verify the binary is installed                |
| `think-and-ship --version`           | Show version info                             |

The `--http` flag accepts `host:port`, `:port`, or a bare `port` (defaults
to `127.0.0.1`). The MCP endpoint is mounted at `/mcp`:

```sh
think-and-ship serve --http :8080
# → think-and-ship http on http://127.0.0.1:8080/mcp
```

## Specification compliance

think-and-ship targets **MCP `2025-06-18`** (via rmcp 1.7). Both stdio
and Streamable HTTP transports advertise this protocol version on
`initialize`.

### What we don't yet have from `2025-11-25`

The November 2025 interim spec added several capabilities we don't
implement yet — most of them gate on rmcp catching up:

| Capability                       | Status                                 |
|----------------------------------|----------------------------------------|
| Tasks (durable requests, SEP-1686) | Pending rmcp support                 |
| Icons on tools/resources (SEP-973) | Pending rmcp support                 |
| OIDC discovery for auth servers   | Pending rmcp support; we ship unauth   |
| Elicitation redesign (SEP-1330)   | Pending rmcp support                   |
| Tool calling in sampling (SEP-1577) | Pending rmcp support                 |
| JSON Schema 2020-12 default       | ✅ already met (schemars 1.x default)  |

### `2026-07-28` Release Candidate readiness

The RC is a breaking spec revision: stateless transport (no
`Mcp-Session-Id`, no `initialize` handshake), `_meta`-envelope routing,
multi-round-trip requests, hardened OAuth, an Extensions framework, and
the `-32002` → `-32602` error-code flip for missing resources.

**Existing v0.2.0 deployments do not break.** SEP-2596 guarantees a
**≥12-month deprecation window** between a spec being marked deprecated
and being removed, so a `2025-06-18` server stays valid against any
`2026-07-28`-aware client for at least a year after the new spec ships.

When [rust-sdk#526](https://github.com/modelcontextprotocol/rust-sdk/issues/526)
lands (SEP-1442 statelessness), the migration on our side is
expected to be one wiring change in `cli/mod.rs` — the application-level
session id (which keys persistence and broadcast files) is independent
of the protocol session and continues unchanged.

## Remote deployment

The Streamable HTTP transport is meant for remote MCP clients (browser
extensions, hosted agents, edge workers). Two env vars gate it for
public-facing use; both default to safe loopback-only behavior.

### Docker quickstart

```sh
docker build -f docs/deploy/Dockerfile -t think-and-ship:0.2.0 .
docker run --rm -p 8080:8080 -v ts-data:/data think-and-ship:0.2.0
# → think-and-ship http on http://0.0.0.0:8080/mcp
```

The image is a multi-stage `rust:1.88-slim` → `debian:bookworm-slim`
build with a non-root `think` user and persistence on by default to
`/data`. See [`docs/deploy/Dockerfile`](docs/deploy/Dockerfile) for the
full build and verification commands.

### Host validation (DNS-rebinding protection)

By default the server only accepts requests whose `Host` header is
`localhost`, `127.0.0.1`, or `::1` — the rmcp transport ships this
protection against DNS-rebinding attacks against locally running MCP
servers. Public deployments override the list with their own hostnames:

```sh
THINK_AND_SHIP_HTTP_ALLOWED_HOSTS=mcp.example.com,mcp.example.com:8080
```

> ⚠️ The list **replaces** the default — if you want browsers on the
> same machine to still hit `http://localhost:8080/mcp`, include
> `localhost,127.0.0.1` explicitly:
> `THINK_AND_SHIP_HTTP_ALLOWED_HOSTS=mcp.example.com,localhost,127.0.0.1`

### CORS (browser MCP clients)

Origin validation is **disabled** by default (the rmcp transport ignores
the `Origin` header when the allowlist is empty), which is the right call
for non-browser clients. Browser-based MCP clients send `Origin`, so you
need to enumerate the ones you trust:

```sh
THINK_AND_SHIP_HTTP_ALLOWED_ORIGINS=https://app.example.com,http://localhost:5173
```

Entries must include the scheme. Requests carrying an `Origin` that
isn't on the list are rejected; requests with no `Origin` (e.g. `curl`,
non-browser SDKs) still pass.

The server logs both lists at startup so you can confirm what was
picked up:

```
http allowed hosts: ["mcp.example.com", "localhost", "127.0.0.1"]
http allowed origins: ["https://app.example.com"]
think-and-ship http on http://0.0.0.0:8080/mcp
```

## Architecture

For the full design contract — crate layout, `ToolFamily` trait, typed
`CrossRef` enum, subcommand binary, persistence/broadcast layout,
migration story, SOLID checklist — see
[**docs/ARCHITECTURE.md**](docs/ARCHITECTURE.md).

### Project identity

`project_id = <dir-basename>-<fnv1a_6hex(canonicalized_cwd)>`. Override
with `THINK_AND_SHIP_PROJECT_NAME`. Both tool families resolve the same
id from the working directory.

### Persistence

Atomic JSON files under one XDG data root, partitioned by family:

```
~/.local/share/think-and-ship/
├── think/sessions/<project_id>.json     # reasoning traces
└── ship/sessions/<project_id>.json      # execution traces
```

v0.1.x sessions auto-migrate from `~/.local/share/{deliberate,resolute}-mcp/`
on the first run of `think-and-ship serve`.

### Broadcast

One NDJSON-over-Unix-socket stream with `family` tags so a single viewer
can interleave think + ship events:

```
THINK_AND_SHIP_BROADCAST_PATH=~/.local/share/think-and-ship/broadcast.sock

# Each line:
{ "family": "think", "type": "step_appended", ... }
{ "family": "ship",  "type": "action_recorded", ... }
```

## `think_*` tools

| Tool                          | Purpose                                              |
|-------------------------------|------------------------------------------------------|
| `think_record_step`           | Record a reasoning step                              |
| `think_revise_estimate`       | Adjust step-count estimate                           |
| `think_pin_step`              | Pin a load-bearing conclusion                        |
| `think_set_branch_status`     | Mark a branch active / merged / abandoned            |
| `think_trace_checkpoint`      | Trace-wide health diagnostics                        |
| `think_get_step`              | Fetch a specific step                                |
| `think_search_trace`          | Search across the trace                              |
| `think_step_impact`           | Blast radius of revising a step                      |
| `think_engine_status`         | Engine introspection                                 |
| `think_export_trace`          | Export in markdown / JSON / console                  |
| `think_wipe_trace`            | Wipe everything (destructive)                        |

## `ship_*` tools

| Tool                  | Purpose                                                |
|-----------------------|--------------------------------------------------------|
| `ship_set_objective`  | Define goal + acceptance criteria                      |
| `ship_plan`           | Add / remove / reorder tasks                           |
| `ship_start`          | Begin work on a task                                   |
| `ship_record`         | Log an action (code, test, debug, research, review)    |
| `ship_complete`       | Close a task with artifacts                            |
| `ship_block`          | Mark a task blocked                                    |
| `ship_check`          | Record a quality gate (test, lint, build, review)      |
| `ship_finalize`       | Finalize the objective and emit the ship report        |
| `ship_status`         | Full state snapshot (recovery after context loss)      |
| `ship_export`         | Export the execution trace                             |
| `ship_reset`          | Wipe everything (destructive)                          |

> The 11 `deliberate_*` and 11 `resolute_*` legacy names are still
> served as deprecated aliases of the canonical names above; they will
> stop working in v0.3.0. The one non-1:1 alias is `resolute_ship` →
> `ship_finalize`.

## Environment variables

| Variable                                | Default                                              | Effect                                                  |
|-----------------------------------------|------------------------------------------------------|---------------------------------------------------------|
| `THINK_AND_SHIP_PERSIST`                | `false`                                              | Enable disk persistence                                 |
| `THINK_AND_SHIP_DATA_DIR`               | `~/.local/share/think-and-ship/`                     | Override the XDG data root                              |
| `THINK_AND_SHIP_BROADCAST_PATH`         | _(disabled)_                                         | Unix socket for live broadcast                          |
| `THINK_AND_SHIP_PROJECT_NAME`           | _(from cwd)_                                         | Override project identity                               |
| `THINK_AND_SHIP_AUTO_SESSION`           | `false`                                              | Default session id falls back to the stable `project_id` |
| `THINK_AND_SHIP_DEFAULT_SESSION_ID`     | _(unset)_                                            | Explicit session id override                            |
| `THINK_AND_SHIP_HTTP_ALLOWED_HOSTS`     | `localhost,127.0.0.1,::1`                            | Comma-separated `Host` allowlist for `--http`; replaces the loopback default |
| `THINK_AND_SHIP_HTTP_ALLOWED_ORIGINS`   | _(disabled — `Origin` ignored)_                      | Comma-separated CORS allowlist for browser MCP clients; each entry must include scheme |

Legacy `DELIBERATE_*` and `RESOLUTE_*` env vars are still accepted —
the server logs one deprecation warning per legacy var seen and maps it
onto the new name. The canonical name always wins if both are set.

## Migrating from v0.1.x

If you had `deliberate-mcp` + `resolute-mcp` v0.1.x installed:

1. `npm install -g think-and-ship` (or `cargo install think-and-ship`)
2. Replace the two `mcpServers` entries in `.mcp.json` with the single
   `think-and-ship` entry shown under [Configure](#configure)
3. Restart your MCP client.

That's it. Persisted session files auto-migrate on first run. Existing
prompts using `deliberate_*` / `resolute_*` tool names keep working as
deprecated aliases through v0.2.x.

Full migration notes: [CHANGELOG.md](CHANGELOG.md#020--2026-05-27) and
[docs/RELEASE_NOTES_v0.2.0.md](docs/RELEASE_NOTES_v0.2.0.md).

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The unified server is self-hosting: its development is tracked using
`think_*` and `ship_*` tools by the agent working on the code. The full
trace lives in `ROADMAP.md` (gitignored) and the deliberate / resolute
session files under `~/.local/share/think-and-ship/`.

## Contributing

Pull requests welcome. Please run the test, clippy, and fmt commands
above before submitting. The `docs/ARCHITECTURE.md` contract should
match the implementation; if a PR changes architecture, update both in
the same commit.

## License

[MIT](LICENSE)
