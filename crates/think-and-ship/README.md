# think-and-ship

[![crates.io](https://img.shields.io/crates/v/think-and-ship)](https://crates.io/crates/think-and-ship)
[![docs.rs](https://img.shields.io/docsrs/think-and-ship)](https://docs.rs/think-and-ship)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/AlrikOlson/think-and-ship/blob/main/LICENSE)

A single MCP server that gives a coding agent a structured, cross-referenced
audit trail of **why** it acted and **what** it shipped. Four tool families
share one server and auto-correlate by project:

- **`think_*`** — the reasoning trace: steps, branches, revisions, confidence,
  pinned conclusions. *Why* the agent is doing something.
- **`ship_*`** — the execution trace: objectives, task plans, actions, quality
  gates, artifacts. *What* it did and whether it passed.
- **`roadmap_*`** — the long-horizon plan-of-plans. An ordered set of *chunks*
  (phases); each is realized by a `ship_*` objective and motivated by `think_*`
  reasoning, so the three fuse into one graph.
- **`signal_*`** — stakeholder signals: what people asked for, researched and
  promoted into the roadmap with provenance.

## Install

```sh
cargo install think-and-ship
```

This installs the `think-and-ship` binary (an MCP server + setup CLI). A prebuilt
npm distribution is also available: `npm install -g think-and-ship`.

## Configure

`think-and-ship init` detects your editor and writes its MCP config — **Cursor**
(`.cursor/mcp.json`), **Windsurf** (`.windsurf/mcp.json`), or **Claude Code**
(`.mcp.json`, the default). `--full` also writes a `CLAUDE.md` tool reference.

```sh
cd your-project
think-and-ship init          # or: init --full / --dry-run / --force
```

The entry it writes (add it by hand if you prefer):

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

`THINK_AND_SHIP_PERSIST=true` turns on cross-session disk persistence (atomic
JSON under `$XDG_DATA_HOME/think-and-ship/`). Without it, state is in-memory only.

## The loop

```
think:    think_record_step → think_pin_step → think_trace_checkpoint
ship:     ship_set_objective → ship_plan → ship_start → ship_record → ship_check → ship_finalize
roadmap:  roadmap_next → roadmap_start_chunk → (a ship objective) → roadmap_complete_chunk
```

Cross-reference the families to fuse the trace: a `think_record_step` takes an
`execution_ref: "task:<id>"`; a `ship_record` takes a `think_step: <n>`.
Both halves resolve the same project id from the working directory, so traces
from different conversations in the same project correlate.

44 canonical tools (11 `think_*` + 11 `ship_*` + 12 `roadmap_*` + 10
`signal_*`).

## CLI

| Command | What it does |
|---|---|
| `think-and-ship serve` | Run as an MCP server on stdio |
| `think-and-ship serve --http :8080` | Run over Streamable HTTP (endpoint at `/mcp`) |
| `think-and-ship init [--full] [--dry-run] [--force]` | Write the IDE MCP config (+ optional `CLAUDE.md`) |
| `think-and-ship doctor` | Diagnose setup issues |
| `think-and-ship status` | Project info + config state |
| `think-and-ship roadmap next` | The next ready chunk (unblocked, deps done, most urgent = smallest priority number) |
| `think-and-ship roadmap export [--format markdown\|json]` | Render the roadmap as a `ROADMAP.md`-shaped view |
| `think-and-ship roadmap import [--file F] [--merge] [--dry-run]` | Seed roadmap chunks from a markdown/YAML roadmap |
| `think-and-ship trace promote --session <id> [--step N] [--kind K]` | Promote private git-native trace records to the shared partition |

## Library

The crate also exposes the engines and infra as a library (`think_and_ship`),
so the three traces can be embedded directly:

```sh
cargo add think-and-ship
```

## More

Full tool reference, Streamable HTTP / remote deployment, bearer-token auth, and
git-native team trace sharing are documented in the
[repository README](https://github.com/AlrikOlson/think-and-ship).

## License

[MIT](https://github.com/AlrikOlson/think-and-ship/blob/main/LICENSE)
