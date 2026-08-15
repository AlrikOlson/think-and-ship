# think-and-ship

[![CI](https://github.com/AlrikOlson/think-and-ship/actions/workflows/ci.yml/badge.svg)](https://github.com/AlrikOlson/think-and-ship/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/think-and-ship)](https://www.npmjs.com/package/think-and-ship)
[![crates.io](https://img.shields.io/crates/v/think-and-ship)](https://crates.io/crates/think-and-ship)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

One MCP server — a process an AI agent calls tools on over the Model
Context Protocol. It serves four families of tools, each a prefix and a
concern: `think_*` records *why* the agent is doing something, `ship_*`
records *what* it did and whether it passed its quality gates, `roadmap_*`
holds the long-horizon plan that drives both, and `signal_*` captures what
stakeholders asked for. All four derive one project id from the working
directory, so the records cross-reference into a single audit trail.

## Why would an agent write down what it was thinking?

Because the reasoning dies with the conversation. The diff survives; the
reason for the diff does not. The next session re-derives a decision the
last one already made — or quietly reverses it. A reviewer reading
yesterday's agent change cannot tell a deliberate tradeoff from an
accident. think-and-ship prevents that by making the reasoning and the
execution durable records: each `think_*` step names the task it
motivated, each `ship_*` action names the step that motivated it, and a
quality gate recorded with a command carries the real exit code — the
server runs the command itself, so an agent cannot report a failing suite
as green.

## Quickstart

```sh
cargo install think-and-ship
cd your-project
think-and-ship init --full
```

Done. Binary installed, MCP config written for your IDE, CLAUDE.md
generated with a tool reference. Open a conversation and go.

Auto-detects **Claude Code**, **Cursor**, and **Windsurf**.

## What a trace looks like

The agent records a reasoning step, does the work, and records the gate:

```
think_record_step:
  purpose:       "Pick a retry strategy for the tracker push"
  thought:       "Naive retry re-sends the whole batch; the API is
                  idempotent per issue, so retry per item instead."
  execution_ref: "task:tracker-retry"     # ties the reasoning to a task

ship_check:
  name:    "cargo test"
  command: "cargo test"                   # the server runs it itself
  → passed: true, verified: true, exit_code: 0
```

Both records persist under one project id. Six months later the trace
answers the question the diff cannot: not what changed, but why — and
whether the gates were actually green when it shipped.

## Install

```sh
# cargo (from crates.io — requires Rust, https://rustup.rs)
cargo install think-and-ship

# verify
think-and-ship --version
```

The release pipeline builds prebuilt binaries for five targets — macOS
arm64/x64, Linux arm64/x64, and Windows x64 (`think-and-ship.exe`) — and
attaches them to the GitHub release alongside a `SHA256SUMS` file; the npm
package's postinstall downloads the one matching your platform and refuses
any tarball whose checksum does not match.

The npm registry still serves v0.1.1, which predates the unified server
and fails on install. It drifted that far behind because the release
pipeline could publish to crates.io without ever publishing to npm, and
nothing reported the gap; both halves are now gated (`versions` in CI,
plus the `Registry parity` workflow). The next release run lands npm at
parity with crates.io. Until it does, install with cargo.

### Windows

No toolchain needed once a release carries binaries: the
`think-and-ship-vX.Y.Z-x86_64-pc-windows-msvc.tar.gz` asset holds a
prebuilt `think-and-ship.exe` (Windows 10+ extracts it with the built-in
`tar`), and the npm package installs it for you.

Building from source instead? `cargo install` needs the MSVC linker:
install the
[Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
with the **Desktop development with C++** workload (rustup offers to set
this up on first run). The binary lands at
`%USERPROFILE%\.cargo\bin\think-and-ship.exe`.

GUI-launched MCP clients do not inherit your shell's `PATH`, so write the
config with the absolute path to the exe. Claude Desktop reads
`%APPDATA%\Claude\claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "think-and-ship": {
      "command": "C:\\Users\\you\\.cargo\\bin\\think-and-ship.exe",
      "args": ["serve"],
      "env": { "THINK_AND_SHIP_PERSIST": "true" }
    }
  }
}
```

Data lives under `%APPDATA%\think-and-ship` (in a shell that sets `HOME`,
such as Git Bash, the `$HOME/.local/share/think-and-ship` path wins so
existing data stays where it is); `THINK_AND_SHIP_DATA_DIR` overrides
either. The test and lint suites run on `windows-latest` in CI.

## Documentation

The full index is [docs/README.md](docs/README.md), organised by what you
came for: a tutorial, a how-to guide, reference, or explanation. Highlights:

| Document | What it covers |
|----------|----------------|
| [docs/TUTORIAL.md](docs/TUTORIAL.md) | A first session, end to end — finishes with the trace you produced, in a viewer |
| [docs/WHY_RECORD_REASONING.md](docs/WHY_RECORD_REASONING.md) | The argument for recording reasoning at all |
| [docs/TOOLS.md](docs/TOOLS.md) | All 44 tools, cross-references, persistence, environment variables |
| [docs/WORKFLOWS.md](docs/WORKFLOWS.md) | Roadmap-driven and signal-driven development; the bundled agent skills |
| [docs/CLI.md](docs/CLI.md) | Every CLI command, the generated MCP config, call counts |
| [docs/OBSERVABILITY.md](docs/OBSERVABILITY.md) | OpenTelemetry export, live emission, joining the caller's trace |
| [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) | Remote HTTP deployment, auth, MCP spec compliance |
| [docs/SHARED_TRACES.md](docs/SHARED_TRACES.md) | Mirroring traces into your repo as git-native Agent Trace records |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate layout and the design contract |
| [docs/MIGRATION.md](docs/MIGRATION.md) | Moving an existing skills install to the current destination and profile |
| [CHANGELOG.md](CHANGELOG.md) | Version history, including the v0.3.0 legacy-alias removal |

## Development

Build gates, MSRV, and the hook install step live in
[CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities via
[SECURITY.md](SECURITY.md). The server is self-hosting: its own
development is tracked with the `think_*`, `ship_*`, and `roadmap_*`
tools by the agent working on the code.

## License

[MIT](LICENSE)
