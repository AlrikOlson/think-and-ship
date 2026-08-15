# think-and-ship

[![npm](https://img.shields.io/npm/v/think-and-ship)](https://www.npmjs.com/package/think-and-ship)
[![crates.io](https://img.shields.io/crates/v/think-and-ship)](https://crates.io/crates/think-and-ship)

One MCP server for AI agents, four tool families: `think_*` records why,
`ship_*` records what, `roadmap_*` holds the plan, `signal_*` holds what
people asked for.

## Quickstart

```sh
npm install -g think-and-ship
cd your-project
think-and-ship init --full
```

Server installed, MCP config written for your IDE, CLAUDE.md generated with
a tool reference. Start a conversation and go.

## How the install works

The postinstall fetches a prebuilt binary for your platform when a GitHub
release carries one (macOS arm64/x64, Linux x64/arm64, Windows x64).
Otherwise it builds the matching version from
[crates.io](https://crates.io/crates/think-and-ship) with cargo. If neither
lane is available the package still installs; `think-and-ship --check`
tells you what is missing and how to fix it.

## CLI

| Command | What it does |
|---------|-------------|
| `init` | Write MCP config for your IDE |
| `init --full` | MCP config + CLAUDE.md tool reference |
| `init --dry-run` | Preview without writing |
| `init --force` | Overwrite existing config |
| `skills install` | Install the bundled agent skills |
| `roadmap next` | The next ready chunk to work on |
| `doctor` | Diagnose setup issues |
| `status` | Show project info |
| `--check` | Verify the server binary is installed |
| `--version` | Show wrapper + server version |

Anything else is forwarded to the server binary — `think-and-ship serve` is
what MCP clients invoke.

## Session data

Existing session data migrates
automatically on first run.

[Full documentation](https://github.com/AlrikOlson/think-and-ship)

## License

MIT
