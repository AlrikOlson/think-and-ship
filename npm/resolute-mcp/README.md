# resolute-mcp

[![npm](https://img.shields.io/npm/v/resolute-mcp)](https://www.npmjs.com/package/resolute-mcp)

MCP server for structured execution tracking. The agent records *what* it did — objectives, task plans, actions, quality gates, artifacts.

Part of [think-and-ship](https://github.com/AlrikOlson/think-and-ship). Pairs with [deliberate-mcp](https://www.npmjs.com/package/deliberate-mcp) for reasoning traces.

## Install

```sh
npm install -g resolute-mcp
```

Or install both servers at once: `npm install -g think-and-ship`

## Configure

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

11 tools under the `resolute_` prefix:

`resolute_set_objective` | `resolute_plan` | `resolute_start` | `resolute_record` | `resolute_complete` | `resolute_block` | `resolute_check` | `resolute_ship` | `resolute_status` | `resolute_export` | `resolute_reset`

[Full documentation](https://github.com/AlrikOlson/think-and-ship)

## License

MIT
