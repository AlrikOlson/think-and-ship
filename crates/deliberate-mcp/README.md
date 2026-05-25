# deliberate-mcp

An MCP server that records structured, branching, revisable reasoning traces — written in Rust, distributed as a single static binary.

The model calls one tool — `deliberate_record_step` — once per reasoning step. Each step captures the purpose, context, current thought, expected outcome, planned next action, and rationale. Steps can revise earlier steps, branch off alternative approaches, track confidence, declare dependencies, and be grouped into sessions.

The server exposes 11 tools total, all under the `deliberate_` namespace. Every tool ships with [MCP 2025-06-18](https://modelcontextprotocol.io/specification/2025-06-18/server/tools) tool annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, `title`) so 2026 clients can gate auto-approval, and every JSON-returning tool advertises an `outputSchema` and emits `structuredContent` so agents can parse responses without regex on prose.

## Installation

### From a pre-built release (recommended)

Download the archive for your platform from the [Releases](https://github.com/AlrikOlson/deliberate-mcp/releases) page, extract it, and place the `deliberate-mcp` binary on your `PATH`.

### From source

```sh
cargo install --git https://github.com/AlrikOlson/deliberate-mcp
```

Or, if you've cloned the repo:

```sh
cargo install --path .
```

This places `deliberate-mcp` in `~/.cargo/bin`.

## Configuring an MCP client

Add `deliberate` to your client's MCP configuration:

```json
{
  "mcpServers": {
    "deliberate": {
      "type": "stdio",
      "command": "deliberate-mcp"
    }
  }
}
```

### Environment variables

| Variable | Default | Effect |
|---|---|---|
| `DELIBERATE_STRICT_MODE` | `false` | When `true`, enforces thought prefixes, rationale `"To "` prefix, and standard purposes only. |
| `MAX_HISTORY_SIZE` | `100` | Maximum number of steps kept in memory before the oldest are trimmed. |
| `DELIBERATE_OUTPUT_FORMAT` | `console` | Output format for the stderr trace: `console`, `json`, or `markdown`. |
| `DELIBERATE_NO_COLOR` | `false` | When `true`, disables ANSI colors in the console trace. |
| `DELIBERATE_SESSION_TIMEOUT` | `60` | Session inactivity timeout, in minutes. |
| `DELIBERATE_MAX_BRANCH_DEPTH` | `5` | Maximum nesting of branches off branches. |
| `DELIBERATE_ENABLE_SESSIONS` | `false` | When `true`, `session_id` on a step groups it into a per-session history. |
| `DELIBERATE_RECENT_STEPS_LIMIT` | `3` | How many prior steps are echoed in each `deliberate` response's `recent_steps` rollup. Pinned steps are folded in regardless. |
| `DELIBERATE_PERSIST` | `false` | When `true`, the default history and every named session are loaded from disk on startup and written atomically after every mutation. |
| `DELIBERATE_DATA_DIR` | `${XDG_DATA_HOME:-$HOME/.local/share}/deliberate-mcp` | Override the directory used for persisted session files. |

## Tools

| Tool | Purpose | Hints |
|---|---|---|
| `deliberate_record_step` | Record one reasoning step | mutating |
| `deliberate_engine_status` | Inspect engine state | read-only, idempotent |
| `deliberate_export_trace` | Export trace as markdown/json/console/tree | read-only, idempotent |
| `deliberate_get_step` | Fetch one step by `step_number` | read-only, idempotent |
| `deliberate_search_trace` | Substring search across every step | read-only, idempotent |
| `deliberate_step_impact` | Upstream/downstream/revision graph | read-only, idempotent |
| `deliberate_pin_step` | Pin/unpin an anchor step | mutating, idempotent |
| `deliberate_revise_estimate` | Adjust `estimated_total` on the last step | mutating |
| `deliberate_set_branch_status` | Mark a branch active/merged/abandoned | mutating, idempotent |
| `deliberate_trace_checkpoint` | Whole-trace metacognitive snapshot | read-only, idempotent |
| `deliberate_wipe_trace` | Wipe steps, branches, sessions, persisted files | **destructive** |

All tools advertise `openWorldHint: false` (engine-local). All JSON-returning tools have an `outputSchema`; `deliberate_export_trace` is the one exception since its output format is selected at call time.

## The `deliberate_record_step` tool

Required arguments:

- `step_number` (integer, ≥ 1)
- `estimated_total` (integer, ≥ 1) — current best guess; adjust as you learn more
- `purpose` (string) — standard values: `analysis`, `action`, `reflection`, `decision`, `summary`, `validation`, `exploration`, `hypothesis`, `correction`, `planning`. Custom strings are accepted in flexible mode.
- `context` (string) — what's already known
- `thought` (string) — current reasoning
- `outcome` (string) — what this step produces
- `next_action` (string or structured object) — what to do next
- `rationale` (string) — why you chose that next action

Optional arguments:

- `confidence` (number, 0–1)
- `uncertainty_notes` (string)
- `revises_step` (integer) — step number being revised; the original is annotated with `revised_by`
- `revision_reason` (string)
- `is_final_step` (boolean) — explicit completion flag
- `branch_from` (integer) — fork off a step into an alternative path
- `branch_id`, `branch_name` (strings)
- `tools_used` (array of strings)
- `external_context` (object) — opaque per-step context payload
- `dependencies` (array of integers) — earlier step numbers this depends on
- `session_id` (string) — present only when sessions are enabled

The tool returns a JSON summary: `step_number`, `estimated_total`, `completed`, `total_steps`, `next_action`, and optionally `confidence`, `revised_step`, `branch`.

## Examples

A first step:

```json
{
  "step_number": 1,
  "estimated_total": 4,
  "purpose": "analysis",
  "context": "Investigating why login fails for users with SSO",
  "thought": "Authentication callbacks need to be checked first",
  "outcome": "Identified the OAuth flow as a likely failure point",
  "next_action": "Read the callback handler in auth/sso.ts",
  "rationale": "That's where the redirect lands after the IdP"
}
```

A revision step that corrects an earlier conclusion:

```json
{
  "step_number": 4,
  "estimated_total": 5,
  "purpose": "correction",
  "context": "Re-examining step 2's claim about token validation",
  "thought": "Wait — the token IS validated, but only on POST routes",
  "outcome": "Updated understanding of the auth layer",
  "next_action": "Audit which routes skip token validation",
  "rationale": "To find the gap that lets bad tokens through",
  "revises_step": 2,
  "revision_reason": "Misread the middleware order"
}
```

A branch exploring an alternative:

```json
{
  "step_number": 5,
  "estimated_total": 8,
  "purpose": "exploration",
  "context": "What if we replace the OAuth library entirely?",
  "thought": "Trying a different approach before committing to the patch",
  "outcome": "Sketched the API surface of `oauth2-rs`",
  "next_action": "Estimate migration effort",
  "rationale": "To compare against fixing in place",
  "branch_from": 3,
  "branch_name": "Replace OAuth library"
}
```

## Development

```sh
cargo build              # debug build
cargo test               # run unit + integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo run --release      # run the server on stdio for manual testing
```

### Source layout

```
src/
├── domain/         Pure data types (DeliberateStep, Branch, History, …)
├── engine/         ReasoningServer + behavior (13 concern-focused modules)
│   ├── core.rs           struct, constructors, accessors, persist_active
│   ├── process.rs        process_step + warnings + trim + make_error
│   ├── validation.rs     required-fields, deps, confidence + recovery method
│   ├── recovery.rs       byte-level XML-injection extractors
│   ├── sessions.rs       lifecycle + clock helpers
│   ├── branching.rs      branch creation + depth
│   ├── revisions.rs      back-pointer bookkeeping
│   ├── numbering.rs      project-wide step-number bookkeeping
│   ├── lookup.rs         step_by_number + search
│   ├── impact.rs         dependency-graph walks
│   ├── snapshots.rs      read-only JSON aggregations
│   ├── mutations.rs      revise/pin/branch-status/clear
│   └── export.rs         format dispatch
├── mcp/            MCP wire adapter (DeliberateService, 11 handlers)
├── output_schemas/ Per-tool structuredContent response types
├── persistence.rs  atomic session-file IO
├── broadcast.rs    NDJSON-over-Unix-socket fan-out to the Tauri viewer
├── formatter.rs    step pretty-printing (markdown/console/json)
├── util/text.rs    UTF-8-safe excerpt/truncate helpers
├── config.rs       DeliberateConfig + env-var resolution
└── constants.rs    validation tables
```

Back-compat shims `server.rs`, `tool.rs`, `types.rs` re-export from
`engine/`, `mcp/`, and `domain/` so the Tauri viewer at `app/src-tauri/`
keeps working without an import sweep.

## License

MIT.
