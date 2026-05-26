# think-and-ship

Two MCP servers for AI agents. One thinks, one ships.

**think-and-ship** installs both servers with a single command — no Rust toolchain required.

| Server | What it tracks | Tools |
|--------|---------------|-------|
| **deliberate-mcp** | Structured reasoning — steps, branches, revisions, confidence | 11 under `deliberate_` |
| **resolute-mcp** | Structured execution — objectives, plans, tasks, checks, artifacts | 11 under `resolute_` |

## Quickstart

```sh
npm install -g think-and-ship
cd your-project
think-and-ship init --full
```

That's it. Both servers installed, MCP config written for your IDE, CLAUDE.md generated with tool reference. Start a conversation and go.

## CLI

```
think-and-ship init               Set up MCP config for the current project
think-and-ship init --full        MCP config + CLAUDE.md in one shot
think-and-ship init --with-claude-md  Also generate CLAUDE.md with tool reference
think-and-ship init --dry-run     Show what would be written without writing
think-and-ship init --force       Overwrite existing config
think-and-ship doctor             Diagnose setup issues
think-and-ship status             Show project info and config state
think-and-ship --check            Verify both servers are installed
think-and-ship --version          Show version info for all components
```

## How it works

When you `npm install think-and-ship`, it pulls in both server packages as dependencies. Each package downloads a prebuilt binary for your platform (macOS arm64/x64, Linux x64) during its own postinstall step. No compilation needed.

`think-and-ship init` detects your IDE (Claude Code, Cursor, Windsurf) and project type (Rust, Node, Python, Go), then writes MCP config with smart defaults and optionally a CLAUDE.md tool reference.

Full documentation: [github.com/AlrikOlson/think-and-ship](https://github.com/AlrikOlson/think-and-ship)

## License

MIT
