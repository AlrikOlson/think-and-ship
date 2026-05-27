# think-and-ship

[![CI](https://github.com/AlrikOlson/think-and-ship/actions/workflows/ci.yml/badge.svg)](https://github.com/AlrikOlson/think-and-ship/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/think-and-ship)](https://www.npmjs.com/package/think-and-ship)
[![crates.io](https://img.shields.io/crates/v/deliberate-mcp)](https://crates.io/crates/deliberate-mcp)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Two MCP servers for AI agents. One records **structured reasoning** (deliberate-mcp), the other records **structured execution** (resolute-mcp). When deployed together, they cross-reference each other automatically — you get a full audit trail from "why did the agent decide this?" to "what did it actually do?"

## Quickstart

```sh
npm install -g think-and-ship
cd your-project
think-and-ship init --full
```

Done. Both servers installed, MCP config written for your IDE, and a CLAUDE.md generated with tool reference. Open a conversation and go.

Works with **Claude Code**, **Cursor**, and **Windsurf** — auto-detected.

## What you get

### deliberate-mcp — the thinking track

Records structured reasoning: steps, branches, revisions, confidence scores, dependencies. The agent writes down *why* it's doing something before it does it.

```
deliberate_record_step → deliberate_pin_step → deliberate_trace_checkpoint
```

11 tools under the `deliberate_` prefix. [Full reference below.](#deliberate-mcp-tools)

### resolute-mcp — the doing track

Records structured execution: objectives, task plans, actions, quality gates, artifacts. The agent tracks *what* it did, *whether it passed*, and *what it shipped*.

```
resolute_set_objective → resolute_plan → resolute_start → resolute_record → resolute_check → resolute_ship
```

11 tools under the `resolute_` prefix. [Full reference below.](#resolute-mcp-tools)

### Cross-references

The two servers link to each other automatically:

```
deliberate_record_step:
  execution_ref: "task:auth-refactor"    # points to resolute task

resolute_record:
  deliberate_step: 19                    # points to deliberate step #19
```

Both resolve the same project identity from your working directory, so traces from different conversations in the same project are correlated.

## Install

```sh
# npm (recommended — downloads prebuilt binaries, no Rust needed)
npm install -g think-and-ship

# cargo (from crates.io)
cargo install deliberate-mcp resolute-mcp

# verify
think-and-ship --check
```

## Configure

`think-and-ship init` auto-detects your IDE and writes the config:

| IDE | Config file | Detection |
|-----|-------------|-----------|
| Claude Code | `.mcp.json` | default |
| Cursor | `.cursor/mcp.json` | `.cursor/` dir exists |
| Windsurf | `.windsurf/mcp.json` | `.windsurf/` dir exists |

The generated config:

```json
{
  "mcpServers": {
    "deliberate": {
      "command": "deliberate-mcp",
      "env": {
        "DELIBERATE_PERSIST": "true",
        "DELIBERATE_ENABLE_SESSIONS": "true"
      }
    },
    "resolute": {
      "command": "resolute-mcp",
      "env": { "RESOLUTE_PERSIST": "true" }
    }
  }
}
```

## CLI

| Command | What it does |
|---------|-------------|
| `think-and-ship init` | Write MCP config for your IDE |
| `think-and-ship init --full` | MCP config + CLAUDE.md tool reference |
| `think-and-ship init --dry-run` | Preview without writing |
| `think-and-ship init --force` | Overwrite existing config |
| `think-and-ship doctor` | Diagnose setup issues |
| `think-and-ship status` | Show project info and config state |
| `think-and-ship --check` | Verify both servers are installed |
| `think-and-ship --version` | Show version info |

## Architecture

> **Heads-up:** v0.2.0 merges the two servers into a single `think-and-ship`
> MCP server with `think_*` and `ship_*` tool families. The shape below
> describes the shipped v0.1.x layout. For the unified design, see
> [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

### Project identity

Both servers resolve the same `project_id` from your working directory: `<dir-basename>-<fnv1a-hash>`. Override with `DELIBERATE_PROJECT_NAME` or `RESOLUTE_PROJECT_NAME`.

### Persistence

Atomic JSON files under `~/.local/share/`:

```
~/.local/share/deliberate-mcp/sessions/   # reasoning traces
~/.local/share/resolute-mcp/sessions/     # execution traces
```

### Broadcast

Both emit NDJSON over Unix sockets for live viewers:

```
DELIBERATE_BROADCAST_PATH=/tmp/deliberate.sock
RESOLUTE_BROADCAST_PATH=/tmp/resolute.sock
```

## deliberate-mcp tools

| Tool | Purpose |
|------|---------|
| `deliberate_record_step` | Record a reasoning step |
| `deliberate_revise_estimate` | Adjust step count estimate |
| `deliberate_pin_step` | Pin a load-bearing conclusion |
| `deliberate_set_branch_status` | Mark branch active/merged/dead |
| `deliberate_trace_checkpoint` | Trace-wide health diagnostics |
| `deliberate_get_step` | Fetch a specific step |
| `deliberate_search_trace` | Search across the trace |
| `deliberate_step_impact` | Blast radius of revising a step |
| `deliberate_engine_status` | Engine introspection |
| `deliberate_export_trace` | Export in markdown/JSON/console |
| `deliberate_wipe_trace` | Wipe everything (destructive) |

## resolute-mcp tools

| Tool | Purpose |
|------|---------|
| `resolute_set_objective` | Define goal + acceptance criteria |
| `resolute_plan` | Add/remove/reorder tasks |
| `resolute_start` | Begin work on a task |
| `resolute_record` | Log an action (code, test, debug, research) |
| `resolute_complete` | Close a task with artifacts |
| `resolute_block` | Mark a task blocked |
| `resolute_check` | Record a quality gate (test, lint, build, review) |
| `resolute_ship` | Ship the objective, review all checks |
| `resolute_status` | Full state snapshot (recovery after context loss) |
| `resolute_export` | Export trace as markdown or JSON |
| `resolute_reset` | Wipe everything (destructive) |

## Environment variables

### deliberate-mcp

| Variable | Default | Effect |
|----------|---------|--------|
| `DELIBERATE_PERSIST` | `false` | Enable disk persistence |
| `DELIBERATE_DATA_DIR` | `~/.local/share/deliberate-mcp` | Data directory |
| `DELIBERATE_PROJECT_NAME` | _(from cwd)_ | Override project identity |
| `DELIBERATE_ENABLE_SESSIONS` | `false` | Enable session grouping |
| `DELIBERATE_BROADCAST_PATH` | _(disabled)_ | Unix socket for live broadcast |
| `DELIBERATE_STRICT_MODE` | `false` | Enforce formatting rules |

### resolute-mcp

| Variable | Default | Effect |
|----------|---------|--------|
| `RESOLUTE_PERSIST` | `false` | Enable disk persistence |
| `RESOLUTE_DATA_DIR` | `~/.local/share/resolute-mcp` | Data directory |
| `RESOLUTE_PROJECT_NAME` | _(from cwd)_ | Override project identity |
| `RESOLUTE_BROADCAST_PATH` | _(disabled)_ | Unix socket for live broadcast |

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --exclude think-and-ship-viewer -- -D warnings
cargo fmt --all --check
```

## Contributing

Pull requests welcome. Please run the test and lint commands above before submitting.

## License

[MIT](LICENSE)
