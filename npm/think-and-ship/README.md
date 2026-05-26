# think-and-ship

Two MCP servers for AI agents. One thinks, one ships.

**think-and-ship** installs both servers with a single command — no Rust toolchain required.

| Server | What it tracks | Tools |
|--------|---------------|-------|
| **deliberate-mcp** | Structured reasoning — steps, branches, revisions, confidence | 11 under `deliberate_` |
| **resolute-mcp** | Structured execution — objectives, plans, tasks, checks, artifacts | 11 under `resolute_` |

## Install

```sh
npm install -g think-and-ship
```

Verify everything works:

```sh
think-and-ship --check
```

## Configure (Claude Code)

Add to your project's `.mcp.json`:

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

For Cursor, Windsurf, or VS Code — same pattern, different config file location.

## CLI

```
think-and-ship --check       Verify both servers are installed
think-and-ship --version     Show version info
think-and-ship --help        Show help
think-and-ship init          Set up config for your project (coming soon)
```

## How it works

When you `npm install think-and-ship`, it pulls in both server packages as dependencies. Each package downloads a prebuilt binary for your platform (macOS arm64/x64, Linux x64) during its own postinstall step. No compilation needed.

Both servers share project identity via `think-and-ship-core`. When deployed in the same working directory, they auto-correlate and cross-reference each other's data.

## License

MIT
