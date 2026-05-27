# deliberate-mcp — DEPRECATED

[![crates.io](https://img.shields.io/crates/v/deliberate-mcp)](https://crates.io/crates/deliberate-mcp)

> **This crate is deprecated as of v0.3.2.** Its reasoning-trace server has
> merged into the unified [`think-and-ship`](https://crates.io/crates/think-and-ship)
> crate. The binary in this package now prints a redirect message and
> exits non-zero; the library types are preserved for the migration window
> and will be removed in v0.3.3.

## Migrate

```sh
cargo install think-and-ship
# or
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

The 11 `deliberate_*` tool names remain wired as deprecated aliases for
their `think_*` canonicals through v0.2.x of `think-and-ship`. Existing
agent prompts keep working unchanged during the transition.

## Why

See the [v0.2.0 changelog](https://github.com/AlrikOlson/think-and-ship/blob/main/CHANGELOG.md#020--2026-05-27)
and [docs/ARCHITECTURE.md](https://github.com/AlrikOlson/think-and-ship/blob/main/docs/ARCHITECTURE.md)
for the rationale behind the merge.
