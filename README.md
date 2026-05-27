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

| Command                          | What it does                                  |
|----------------------------------|-----------------------------------------------|
| `think-and-ship serve`           | Run as MCP server on stdio                    |
| `think-and-ship init`            | Write MCP config for your IDE                 |
| `think-and-ship init --full`     | MCP config + CLAUDE.md tool reference         |
| `think-and-ship init --dry-run`  | Preview without writing                       |
| `think-and-ship init --force`    | Overwrite existing config                     |
| `think-and-ship doctor`          | Diagnose setup issues                         |
| `think-and-ship status`          | Project info + config state                   |
| `think-and-ship --check`         | Verify the binary is installed                |
| `think-and-ship --version`       | Show version info                             |

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

| Variable                            | Default                                              | Effect                                                  |
|-------------------------------------|------------------------------------------------------|---------------------------------------------------------|
| `THINK_AND_SHIP_PERSIST`            | `false`                                              | Enable disk persistence                                 |
| `THINK_AND_SHIP_DATA_DIR`           | `~/.local/share/think-and-ship/`                     | Override the XDG data root                              |
| `THINK_AND_SHIP_BROADCAST_PATH`     | _(disabled)_                                         | Unix socket for live broadcast                          |
| `THINK_AND_SHIP_PROJECT_NAME`       | _(from cwd)_                                         | Override project identity                               |
| `THINK_AND_SHIP_AUTO_SESSION`       | `false`                                              | Default session id falls back to the stable `project_id` |
| `THINK_AND_SHIP_DEFAULT_SESSION_ID` | _(unset)_                                            | Explicit session id override                            |

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
