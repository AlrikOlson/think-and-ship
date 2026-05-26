# deliberate-mcp

[![crates.io](https://img.shields.io/crates/v/deliberate-mcp)](https://crates.io/crates/deliberate-mcp)
[![npm](https://img.shields.io/npm/v/deliberate-mcp)](https://www.npmjs.com/package/deliberate-mcp)

An MCP server that records structured, branching, revisable reasoning traces. The agent writes down *why* before it acts.

Part of [think-and-ship](https://github.com/AlrikOlson/think-and-ship) — pairs with [resolute-mcp](https://crates.io/crates/resolute-mcp) for execution tracking.

## Install

```sh
# npm (prebuilt binary, no Rust needed)
npm install -g deliberate-mcp

# cargo
cargo install deliberate-mcp
```

## Configure

Add to your MCP client config (`.mcp.json`, `.cursor/mcp.json`, etc.):

```json
{
  "mcpServers": {
    "deliberate": {
      "command": "deliberate-mcp",
      "env": {
        "DELIBERATE_PERSIST": "true",
        "DELIBERATE_ENABLE_SESSIONS": "true"
      }
    }
  }
}
```

Or use the combined installer: `npm install -g think-and-ship && think-and-ship init`

## Tools

| Tool | Purpose |
|------|---------|
| `deliberate_record_step` | Record a reasoning step (the core tool) |
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

All tools have `outputSchema` + `structuredContent` and MCP tool annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`).

## How it works

The agent calls `deliberate_record_step` once per reasoning step. Each step captures:

- **purpose** — what kind of thinking (analysis, decision, hypothesis, correction...)
- **context** — what's already known
- **thought** — current reasoning
- **outcome** — what this step produces
- **next_action** — what to do next
- **rationale** — why

Steps can revise earlier steps, branch into alternatives, track confidence, declare dependencies, and group into sessions. The server maintains a DAG of reasoning that can be searched, exported, and audited.

When paired with resolute-mcp, pass `execution_ref: "task:<id>"` to link reasoning to execution.

## Environment variables

| Variable | Default | Effect |
|----------|---------|--------|
| `DELIBERATE_PERSIST` | `false` | Enable disk persistence |
| `DELIBERATE_DATA_DIR` | `~/.local/share/deliberate-mcp` | Data directory |
| `DELIBERATE_PROJECT_NAME` | _(from cwd)_ | Override project identity |
| `DELIBERATE_ENABLE_SESSIONS` | `false` | Enable session grouping |
| `DELIBERATE_BROADCAST_PATH` | _(disabled)_ | Unix socket for live broadcast |
| `DELIBERATE_STRICT_MODE` | `false` | Enforce formatting rules |

## License

[MIT](../../LICENSE)
