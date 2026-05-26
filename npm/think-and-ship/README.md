# think-and-ship

[![npm](https://img.shields.io/npm/v/think-and-ship)](https://www.npmjs.com/package/think-and-ship)
[![CI](https://github.com/AlrikOlson/think-and-ship/actions/workflows/ci.yml/badge.svg)](https://github.com/AlrikOlson/think-and-ship/actions/workflows/ci.yml)

Two MCP servers for AI agents. One records structured reasoning, the other records structured execution. One command installs both.

## Quickstart

```sh
npm install -g think-and-ship
cd your-project
think-and-ship init --full
```

Both servers installed, MCP config written for your IDE, CLAUDE.md generated with tool reference. Start a conversation and go.

## CLI

| Command | What it does |
|---------|-------------|
| `init` | Write MCP config for your IDE |
| `init --full` | MCP config + CLAUDE.md tool reference |
| `init --dry-run` | Preview without writing |
| `init --force` | Overwrite existing config |
| `doctor` | Diagnose setup issues |
| `status` | Show project info |
| `--check` | Verify both servers installed |
| `--version` | Show versions |

## What's inside

| Package | What it does |
|---------|-------------|
| [deliberate-mcp](https://www.npmjs.com/package/deliberate-mcp) | Structured reasoning traces (11 tools) |
| [resolute-mcp](https://www.npmjs.com/package/resolute-mcp) | Structured execution tracking (11 tools) |

Both download prebuilt binaries for your platform (macOS arm64/x64, Linux x64). No Rust needed.

[Full documentation](https://github.com/AlrikOlson/think-and-ship)

## License

MIT
