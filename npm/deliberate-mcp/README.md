# deliberate-mcp

Structured, branching, revisable reasoning traces over MCP.

Part of [think-and-ship](https://github.com/AlrikOlson/think-and-ship) — pairs with [resolute-mcp](https://github.com/AlrikOlson/think-and-ship/tree/main/crates/resolute-mcp) for execution tracking.

## Install

```sh
npx deliberate-mcp
```

Or with cargo:

```sh
cargo install deliberate-mcp
```

## Configure (Claude Code)

Add to `.mcp.json`:

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

## Tools

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
| `deliberate_export_trace` | Export (markdown/JSON/console) |
| `deliberate_wipe_trace` | Wipe everything |

## License

MIT
