# Changelog

All notable changes to think-and-ship are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project uses
SemVer.

## [0.2.0] — 2026-05-27

### The merge

Two cooperating MCP servers (`deliberate-mcp` v0.3.x + `resolute-mcp` v0.1.x)
become **one unified `think-and-ship` MCP server** with two namespaced tool
families. Driven by AWS DDD guidance — *server boundaries should reflect
agent responsibility, not tool availability* — and a 0-SOLID-violation
audit of the v0.1.x codebase that confirmed the cost of the split was
coordination, not code (two env-var families, two persistence dirs, two
broadcast sockets, two MCP config blocks).

**The unified server:** 22 canonical tools (11 `think_*` reasoning + 11 `ship_*` execution) plus 22 deprecated aliases (`deliberate_*` and `resolute_*`) wired with `_meta.deprecation_warning` per the MCP spec. One stdio binary, one broadcast socket, one persistence root, one MCP config entry.

### Added

- **Unified server (`think-and-ship` crate, v0.2.0)** — single binary exposing both tool families through one `UnifiedService` that routes by name prefix.
- **`think-and-ship serve`** — runs the merged MCP server on stdio.
- **`ship_finalize`** — renamed from `ship_ship` / `resolute_ship` (canonical name; both old names still resolve).
- **Family-tagged broadcast** — one Unix socket emits NDJSON frames with `{ "family": "think" | "ship", ... }` so a single viewer reads both halves.
- **Typed `CrossRef`** — internal enum (`ThinkStep` / `ShipTask` / `ShipAction` / `ShipCheck`) replaces the string-only `execution_ref` at use sites; the wire form (`task:foo`, `action:42`, `check:cargo-test`) is preserved.
- **`ToolFamily` trait + `FamilyRegistry`** — namespaced tool families register via composition (OCP) without modifying the wire adapter.
- **`migrate::migrate_v0_1_data`** — on first run, moves `~/.local/share/{deliberate,resolute}-mcp/sessions/*` into `~/.local/share/think-and-ship/{think,ship}/sessions/*` once, then drops a `.migrated-from-v0.1` marker. Conflicts skip with a warning rather than clobber.
- **`env_compat::translate_legacy_env_vars`** — accepts the legacy `DELIBERATE_*` and `RESOLUTE_*` env vars at startup and maps them to `THINK_AND_SHIP_*` equivalents (one `tracing::warn` per legacy var seen; new name wins when both set).
- **End-to-end rmcp client test** — pairs a real rmcp client with the unified server over `tokio::io::duplex` and verifies tools/list + alias dispatch through actual wire serialization.
- **`docs/ARCHITECTURE.md`** — design contract for the unified architecture.

### Changed

- **`crates/deliberate-mcp` and `crates/resolute-mcp` binaries** reduced to deprecation stubs (print a redirect to `think-and-ship` and exit 1). The library code is preserved for the migration window so existing Rust dependents still compile; v0.3.0 will remove it.
- **`npm/think-and-ship`** rewritten to install the unified Rust binary directly. The two old npm wrappers (`npm/deliberate-mcp`, `npm/resolute-mcp`) become deprecation stubs whose `install.js` and `bin/*` scripts print a redirect.
- **Persistence layout** partitioned: think writes to `<data_dir>/think/sessions/`, ship to `<data_dir>/ship/sessions/`. Pre-merge, both wrote to `<data_dir>/sessions/` and could clobber each other on shared `<project_id>.json` filenames — the dedicated subdirs eliminate that collision.
- **Viewer crate** renamed `deliberate-app` → `think-and-ship-viewer` and moved to `crates/think-and-ship-viewer/`. Reads the single shared broadcast socket and dispatches frames by family tag.
- **Internal module** `crate::engine` (shared infrastructure: project_id, persistence, broadcast, cross_ref) renamed to `crate::infra` to disambiguate from the per-family reasoning engine (`crate::think::engine`) and execution engine (`crate::ship::engine`).
- **Versions** aligned across Cargo / npm within each published package: `think-and-ship` 0.2.0, `deliberate-mcp` 0.3.2 (stub), `resolute-mcp` 0.1.2 (stub).
- **`docs/ARCHITECTURE.md`** auto-session description now matches the actual `<basename>-<6hex>` stable-id behavior (the timestamped form was design intent, never shipped).

### Deprecated

- **`deliberate_*` tool names** — the 11 old reasoning tool names are aliased to their `think_*` canonicals; calls still work and emit `_meta.deprecation_warning` per the MCP spec. To be removed in v0.3.0.
- **`resolute_*` tool names** — same story for the execution side.
- **`ship_ship`** — was a mechanical rename of `resolute_ship` for ~one release cycle; now an alias for `ship_finalize`.
- **`DELIBERATE_*` and `RESOLUTE_*` env vars** — accepted at startup with a translation warning; the canonical names are `THINK_AND_SHIP_*`.
- **`deliberate-mcp` and `resolute-mcp` packages on crates.io / npm** — published as deprecation-stub versions (0.3.2 / 0.1.2). New installs should use `think-and-ship` instead.

### Removed

- **`crates/think-and-ship-core`** — its `project_id` algorithm now lives in `crate::infra::project_id` (folded into the unified crate).
- **Dual broadcast sockets** — single `<data_dir>/broadcast.sock` with family-tagged frames replaces `/tmp/deliberate.sock` + `/tmp/resolute.sock`.

### Fixed

- **Persistence path collision** between think and ship traces sharing a `<project_id>.json` filename under the same `data_dir`.

### Migration

If you were running v0.1.x deliberate-mcp + resolute-mcp:

1. `npm install -g think-and-ship` (or `cargo install think-and-ship`)
2. Replace the two `mcpServers` entries in `.mcp.json` (or your IDE's config) with one entry:
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
3. Existing prompts using `deliberate_*` / `resolute_*` tool names continue to work as deprecated aliases through v0.2.x.
4. Persisted session files under `~/.local/share/{deliberate,resolute}-mcp/sessions/` are auto-migrated to the new layout on first run.

The 244-test pre-merge baseline is preserved across both deprecated crates;
the unified crate adds 284 new tests bringing the workspace to 528 passing.

[0.2.0]: https://github.com/AlrikOlson/think-and-ship/releases/tag/v0.2.0
