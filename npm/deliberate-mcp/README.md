# deliberate-mcp

[![npm](https://img.shields.io/npm/v/deliberate-mcp)](https://www.npmjs.com/package/deliberate-mcp)

MCP server for structured reasoning traces. The agent records *why* before it acts — steps, branches, revisions, confidence, dependencies.

Part of [think-and-ship](https://github.com/AlrikOlson/think-and-ship). Pairs with [resolute-mcp](https://www.npmjs.com/package/resolute-mcp) for execution tracking.

## Install

```sh
npm install -g deliberate-mcp
```

Or install both servers at once: `npm install -g think-and-ship`

## Configure

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

11 tools under the `deliberate_` prefix:

`deliberate_record_step` | `deliberate_pin_step` | `deliberate_trace_checkpoint` | `deliberate_search_trace` | `deliberate_get_step` | `deliberate_step_impact` | `deliberate_revise_estimate` | `deliberate_set_branch_status` | `deliberate_engine_status` | `deliberate_export_trace` | `deliberate_wipe_trace`

[Full documentation](https://github.com/AlrikOlson/think-and-ship)

## License

MIT
