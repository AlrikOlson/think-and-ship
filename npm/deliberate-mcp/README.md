# deliberate-mcp — DEPRECATED

> **This package is deprecated as of v0.3.2.** It has merged into the
> unified [`think-and-ship`](https://www.npmjs.com/package/think-and-ship)
> npm package. Installing this package now prints a redirect message
> during postinstall and exits cleanly without downloading a binary.

## Migrate

```sh
npm uninstall -g deliberate-mcp
npm install -g think-and-ship
```

In your MCP client config (`.mcp.json`, `.cursor/mcp.json`, etc.):

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

The 11 `deliberate_*` tool names continue to work as deprecated aliases
for their `think_*` canonicals through v0.2.x of `think-and-ship`, so
existing agent prompts don't break during the transition.

## Why

See the [project README](https://github.com/AlrikOlson/think-and-ship)
and the [v0.2.0 changelog](https://github.com/AlrikOlson/think-and-ship/blob/main/CHANGELOG.md#020--2026-05-27)
for the rationale behind the merge.
