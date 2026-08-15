# Your first session

> **Tutorial** — a first session, walked end to end ([all docs](README.md)).

This walkthrough takes about fifteen minutes and ends with you looking at a
trace your own agent produced, rendered as a timeline in your browser. You
need a Rust toolchain (for `cargo install`), a coding agent that speaks MCP
(Claude Code, Cursor, or Windsurf), and — for the final step only — Docker.

## 1. Install the binary

```sh
cargo install think-and-ship
```

This puts one binary named `think-and-ship` on your PATH. It is both the
MCP server your agent will call and the CLI you will use to inspect what
the agent recorded.

## 2. Set up a project

Pick a real project — the tutorial works in any directory, but the records
are only interesting if the work is. Then:

```sh
cd your-project
think-and-ship init --full
```

`init` detects your IDE and writes its MCP config (`.mcp.json` for Claude
Code, `.cursor/mcp.json` for Cursor, `.windsurf/mcp.json` for Windsurf) with
persistence turned on, so records survive across sessions. `--full` also
generates a CLAUDE.md tool reference, which is what tells the agent the
tools exist and when to use them. Verify the setup:

```sh
think-and-ship doctor
```

## 3. Let the agent work

Restart your agent so it picks up the new MCP config, then give it a small
real task with one added sentence:

> Fix <something small and real>. Record your reasoning with
> `think_record_step` before you start and after you finish, track the work
> with a `ship_*` objective, and record the test run as a `ship_check` with
> a `command` so the server verifies it.

The agent opens a reasoning step (why it is doing this), sets an objective,
logs its actions, and records the quality gate. The gate matters most:
`ship_check` with a `command` makes the server run the command itself and
store the real exit code, so the record says `verified: true` rather than
taking the agent's word for it.

## 4. See what was recorded

```sh
think-and-ship status
think-and-ship calls
```

`status` shows the project id every record was filed under — all four tool
families derive it from the working directory, which is what makes the
records cross-reference without configuration. `calls` shows how many times
each tool was actually dispatched, from a local count the server keeps per
project.

## 5. Look at the trace

```sh
think-and-ship otel wizard
```

The wizard writes a docker-compose file for a local
[Jaeger](https://www.jaegertracing.io/) (a trace viewer), starts it, and
sends your project's trace to it. Open the URL it prints. What you see is
the session you just ran, as a span tree: the objective at the root, tasks
under it, actions and checks under those, and the reasoning steps parented
to the work they motivated. A failed gate would show as an ERROR span.

No Docker? `think-and-ship trace export --out trace.json` writes the same
trace as OpenTelemetry JSON, which any OTLP-compatible viewer can load.
When you are done looking:

```sh
think-and-ship otel down
```

## Where to go next

- [WORKFLOWS.md](WORKFLOWS.md) — the loops this becomes at project scale:
  roadmap-driven and signal-driven development.
- [WHY_RECORD_REASONING.md](WHY_RECORD_REASONING.md) — the argument for
  doing any of this.
- [TOOLS.md](TOOLS.md) — every tool the agent just used, in full.
