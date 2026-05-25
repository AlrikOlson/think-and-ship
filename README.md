# think-and-ship

Two MCP servers. One thinks, one ships.

| Server | What it tracks | Tools |
|--------|---------------|-------|
| **deliberate-mcp** | Structured reasoning — steps, branches, revisions, confidence, dependencies | 11 under `deliberate_` |
| **resolute-mcp** | Structured execution — objectives, plans, tasks, checks, artifacts | 11 under `resolute_` |

Both servers share project identity via `think-and-ship-core`. When
deployed in the same working directory, they auto-correlate and
cross-reference each other's data.

## Install

```sh
# From source
cargo install --path crates/deliberate-mcp
cargo install --path crates/resolute-mcp
```

## Configure

### Claude Code

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
      "env": {
        "RESOLUTE_PERSIST": "true"
      }
    }
  }
}
```

### Cursor / Windsurf / VS Code

Same pattern — add both servers to your MCP configuration. Both
communicate over stdio.

## How they work together

### Shared project identity

Both servers resolve the same `project_id` from your working directory:

```
<directory-basename>-<fnv1a-hash>
```

Override with `DELIBERATE_PROJECT_NAME` (shared) or
`RESOLUTE_PROJECT_NAME` (resolute-specific).

### Cross-references

When using both servers, link reasoning to execution:

```
deliberate_record_step:
  execution_ref: "task:auth-refactor"    ← points to resolute task

resolute_record:
  deliberate_step: 19                    ← points to deliberate step #19
```

`resolute_status` surfaces all cross-refs so you can trace from
reasoning to execution and back.

### Persistence

Both write atomic JSON files under `~/.local/share/`:

```
~/.local/share/deliberate-mcp/sessions/   ← reasoning traces
~/.local/share/resolute-mcp/sessions/     ← execution traces
```

### Broadcast

Both emit NDJSON over Unix sockets for live viewers:

```
DELIBERATE_BROADCAST_PATH=/tmp/deliberate.sock
RESOLUTE_BROADCAST_PATH=/tmp/resolute.sock
```

## resolute-mcp tools

| Tool | Purpose |
|------|---------|
| `resolute_set_objective` | Define goal + acceptance criteria |
| `resolute_plan` | Add/remove/reorder tasks |
| `resolute_start` | Begin work on a task |
| `resolute_record` | Log an action (code, test, debug, research) |
| `resolute_complete` | Close a task with artifacts |
| `resolute_block` | Mark a task blocked |
| `resolute_check` | Record a quality gate (test, lint, build, review) |
| `resolute_ship` | Ship the objective, review all checks |
| `resolute_status` | Full state snapshot (recovery after context loss) |
| `resolute_export` | Export trace as markdown or JSON |
| `resolute_reset` | Wipe everything (destructive) |

## deliberate-mcp tools

| Tool | Purpose |
|------|---------|
| `deliberate_record_step` | Record a reasoning step |
| `deliberate_revise_estimate` | Adjust step count estimate |
| `deliberate_pin_step` | Pin a load-bearing conclusion |
| `deliberate_set_branch_status` | Mark branch active/merged/dead |
| `deliberate_trace_checkpoint` | Trace-wide health diagnostics |
| `deliberate_get_step` | Fetch a specific step |
| `deliberate_search_trace` | Search across the trace |
| `deliberate_step_impact` | Blast radius of revising a step |
| `deliberate_engine_status` | Engine introspection |
| `deliberate_export_trace` | Export in markdown/JSON/console |
| `deliberate_wipe_trace` | Wipe everything (destructive) |

## Environment variables

### resolute-mcp

| Variable | Default | Effect |
|----------|---------|--------|
| `RESOLUTE_PERSIST` | `false` | Enable disk persistence |
| `RESOLUTE_DATA_DIR` | `~/.local/share/resolute-mcp` | Data directory |
| `RESOLUTE_PROJECT_NAME` | (from cwd) | Override project identity |
| `RESOLUTE_BROADCAST_PATH` | (disabled) | Unix socket path for live broadcast |

### deliberate-mcp

| Variable | Default | Effect |
|----------|---------|--------|
| `DELIBERATE_PERSIST` | `false` | Enable disk persistence |
| `DELIBERATE_DATA_DIR` | `~/.local/share/deliberate-mcp` | Data directory |
| `DELIBERATE_PROJECT_NAME` | (from cwd) | Override project identity |
| `DELIBERATE_ENABLE_SESSIONS` | `false` | Enable session grouping |
| `DELIBERATE_BROADCAST_PATH` | (disabled) | Unix socket path |
| `DELIBERATE_STRICT_MODE` | `false` | Enforce formatting rules |

## Development

```sh
# Run all tests
cargo test --workspace

# Build both binaries
cargo build --workspace --release

# Run resolute-mcp locally
cargo run -p resolute-mcp

# Run deliberate-mcp locally
cargo run -p deliberate-mcp
```

## License

MIT
