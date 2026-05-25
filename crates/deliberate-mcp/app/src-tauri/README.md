# deliberate-app

Live viewer for the `deliberate-mcp` reasoning trace.

A Tauri 2 desktop window with three views — Trace (vertical git-log-style
timeline), Graph (dagre-laid-out dependency DAG), Checkpoint (the five
diagnostics from `deliberate_checkpoint`). Viewer-only: the agent owns
all writes; this app reads.

## How it gets the trace

Two paths, run concurrently:

- **Socket** (real-time). When the MCP server is launched with
  `DELIBERATE_BROADCAST_PATH=/path/to/sock`, it binds a Unix socket and emits
  one NDJSON frame per trace mutation. The viewer reconnects with
  backoff when the server isn't yet up.
- **File** (atomic snapshot). When `DELIBERATE_PERSIST=true` is set, the
  server writes atomic JSON files to `$DELIBERATE_DATA_DIR/sessions/`
  (default `$XDG_DATA_HOME/deliberate-mcp/sessions/`). The viewer's
  `notify`-debounced watcher re-loads any file that changes.

Both feed the same in-memory state and emit the same Tauri events to
the frontend. The status bar reports which sources are alive.

## Run

The `@tauri-apps/cli` is installed as a dev-dependency in `app/package.json`,
so all commands run from `app/`:

```sh
cd app

# One-time, after a fresh clone:
npm install

# Dev: hot-reload frontend + Rust window in one process.
npm run tauri dev

# Production bundle (release build of both halves).
npm run tauri build
```

`tauri dev` reads `app/src-tauri/tauri.conf.json` and starts the Vite
dev server itself via the configured `beforeDevCommand`. You do not
need to start Vite separately.

For a real production bundle, replace the 1×1 placeholder PNGs under
`app/src-tauri/icons/` with real icons (see `icons/README.md`).

## Drive it with a live agent

In one shell, run an MCP client (Claude Code, Cursor, Windsurf, etc.)
pointed at `deliberate-mcp` with both env vars set:

```sh
DELIBERATE_PERSIST=true \
DELIBERATE_BROADCAST_PATH=/tmp/deliberate.sock \
cargo run --release -p deliberate-mcp
```

In another shell, run the viewer with the same `DELIBERATE_BROADCAST_PATH`
exported. Steps appear in the timeline as the agent records them.

If the agent isn't running yet, the viewer shows an empty state with
the resolved paths and reconnects when the socket appears.
