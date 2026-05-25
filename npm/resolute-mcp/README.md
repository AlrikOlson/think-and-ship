# resolute-mcp

Structured execution tracking for autonomous AI development.

Part of [think-and-ship](https://github.com/AlrikOlson/think-and-ship) — pairs with [deliberate-mcp](https://github.com/AlrikOlson/think-and-ship/tree/main/crates/deliberate-mcp) for reasoning traces.

## Install

```sh
npx resolute-mcp
```

Or with cargo:

```sh
cargo install resolute-mcp
```

## Configure (Claude Code)

Add to `.mcp.json`:

```json
{
  "mcpServers": {
    "resolute": {
      "command": "resolute-mcp",
      "env": { "RESOLUTE_PERSIST": "true" }
    }
  }
}
```

## Tools

| Tool | Purpose |
|------|---------|
| `resolute_set_objective` | Define goal + acceptance criteria |
| `resolute_plan` | Add/remove/reorder tasks |
| `resolute_start` | Begin work on a task |
| `resolute_record` | Log an action (accepts `deliberate_step` cross-ref) |
| `resolute_complete` | Close a task with artifacts |
| `resolute_block` | Mark a task blocked |
| `resolute_check` | Record a quality gate result |
| `resolute_ship` | Ship, reviewing all checks |
| `resolute_status` | Full state snapshot (recovery tool) |
| `resolute_export` | Export trace (markdown/JSON) |
| `resolute_reset` | Wipe everything |

## License

MIT
