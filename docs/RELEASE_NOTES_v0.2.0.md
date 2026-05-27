# think-and-ship v0.2.0 — The Merge

`deliberate-mcp` (reasoning) and `resolute-mcp` (execution) become **one
unified MCP server**: `think-and-ship`. Two tool families behind one
binary, one config block, one socket, one persistence root.

## Why

The two halves served one agent responsibility — reason about what to do,
then track doing it — and were always deployed together. The cost of the
split was coordination, not code: two env-var families, two persistence
directories, two broadcast sockets, two MCP config entries. AWS DDD
guidance was decisive: *"server boundaries should reflect agent
responsibility, not tool availability."*

A `ministr_solid` audit confirmed the v0.1.x codebase had **0 SOLID
violations** — this merge isn't refactoring tech debt, it's removing
coordination overhead.

## What's in the box

- **22 canonical tools**, namespaced: 11 `think_*` (reasoning trace) + 11 `ship_*` (execution trace)
- **22 deprecated aliases** for v0.1.x users: 11 `deliberate_*` and 11 `resolute_*` names continue to work, each carrying `_meta.deprecation_warning` per the MCP spec
- **One stdio binary** (`think-and-ship serve`) with both families registered
- **One broadcast socket** at `~/.local/share/think-and-ship/broadcast.sock` with family-tagged NDJSON frames
- **One persistence root** at `~/.local/share/think-and-ship/{think,ship}/sessions/`
- **One MCP config block** replaces the two v0.1.x entries
- **Auto-migration** of v0.1.x persisted state on first run
- **Legacy env vars accepted** with a deprecation log line (`DELIBERATE_*` / `RESOLUTE_*` → `THINK_AND_SHIP_*`)

## Upgrade

If you're running v0.1.x deliberate-mcp + resolute-mcp:

1. Install the unified binary:
   ```sh
   npm install -g think-and-ship
   # or
   cargo install think-and-ship
   ```

2. Replace the two `mcpServers` entries in `.mcp.json` (or your IDE's
   config) with one entry:
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

3. Restart your MCP client. Persisted sessions auto-migrate on first run;
   existing prompts using `deliberate_*` / `resolute_*` tool names keep
   working as deprecated aliases.

Or run `think-and-ship init` in a project directory and the CLI writes
the config for you (detects Claude Code, Cursor, Windsurf).

## Old packages

The v0.1.x crates and npm packages are deprecated with redirect stubs:

- `deliberate-mcp` 0.3.2 (Cargo + npm) — prints a deprecation message and exits
- `resolute-mcp` 0.1.2 (Cargo + npm) — same

Anyone running `cargo install deliberate-mcp` or `npm install deliberate-mcp`
will see the redirect. The packages stay on the registries so existing
build scripts don't break; they just no longer install a working server.

## Tool name changes

99% of tools just gained a new family-prefix alias:

```
deliberate_record_step     → think_record_step
deliberate_pin_step        → think_pin_step
...
resolute_set_objective     → ship_set_objective
resolute_plan              → ship_plan
...
```

One tool got a semantic rename:

```
resolute_ship              → ship_finalize
```

(both names still work; `ship_ship` would have been the mechanical
rename but reads poorly, so it became `ship_finalize`.)

## Architecture

See [docs/ARCHITECTURE.md](https://github.com/AlrikOlson/think-and-ship/blob/main/docs/ARCHITECTURE.md)
for the full design contract: crate layout, `ToolFamily` trait, typed
`CrossRef` enum, subcommand binary, persistence layout, broadcast format,
migration story, SOLID checklist.

## Test surface

Workspace tests: **528 passing** across the unified crate plus the
preserved v0.1.x library code in the deprecated stub crates.
- Unit tests: 36 (engine) + 7 (project_id)
- Think family (ported): 110 engine + 29 formatter + 18 mcp wire + 36 config + 2 broadcast = 195
- Ship family (ported): 32 engine + 9 mcp wire + 4 persistence = 45
- New regression + integration: 1 persistence-no-collision + 3 unified-service + 3 e2e-rmcp-client = 7
- Pre-merge baseline (deliberate-mcp + resolute-mcp library still alive): 244

## Removing the deprecated names

The `deliberate_*` / `resolute_*` tool aliases and the `DELIBERATE_*` /
`RESOLUTE_*` env vars are scheduled for removal in **v0.3.0**. Update
your prompts and configs before then.

## Thanks

Built using the project's own [`deliberate-mcp`](https://github.com/AlrikOlson/think-and-ship)
+ [`resolute-mcp`](https://github.com/AlrikOlson/think-and-ship) servers
to track the merge work itself. Roadmap, design rationale, and the
phase-by-phase implementation trace are committed under `ROADMAP.md`
(gitignored — that's a separate "make the planning visible" tradeoff).

— [Alrik Olson](https://github.com/AlrikOlson)
