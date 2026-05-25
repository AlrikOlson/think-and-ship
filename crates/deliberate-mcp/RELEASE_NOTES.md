# Release notes

## 0.3.0 — 2026-05-22

### Codebase reorganization — non-functional, fully back-compatible

The 0.2 codebase was a flat `src/` of 11 files with `server.rs` at 2884
lines doing the heavy lifting. 0.3 reorganizes into a domain-driven
module hierarchy without changing behavior, the wire protocol, or any
public-facing API.

New layout:

```
src/
├── domain/         Pure data types (was bundled in types.rs)
│   ├── step.rs           DeliberateStep, NextAction, StructuredAction
│   ├── dep_edge.rs       DepEdge
│   ├── branch.rs         Branch, BranchStatus
│   ├── history.rs        DeliberateHistory, HistoryMetadata
│   └── session.rs        SessionEntry
├── engine/         ReasoningServer + behavior (was server.rs)
│   ├── core.rs           Struct, ctors, accessors, remaining methods
│   ├── validation.rs     Required-fields, dependencies, confidence, etc.
│   └── recovery.rs       XML-injection extractors + helpers
├── mcp/            MCP wire adapter (was tool.rs)
│   ├── service.rs        DeliberateService + ServerHandler impl
│   ├── handlers.rs       The 11 #[tool] handlers
│   ├── args.rs           Input-arg structs
│   └── instructions.rs   SERVER_INSTRUCTIONS
├── output_schemas/ Per-tool response types (was output_schemas.rs)
│   ├── record_step.rs    RecordStepOutput + 3 helpers
│   ├── engine_status.rs
│   ├── search_trace.rs
│   ├── step_impact.rs
│   ├── trace_checkpoint.rs
│   ├── mutations.rs      Pin/Revise/SetBranchStatus/WipeTrace
│   └── error_envelope.rs StructuredError
├── util/text.rs    UTF-8-safe excerpt/truncate helpers
├── broadcast.rs    unchanged
├── persistence.rs  unchanged
├── formatter.rs    unchanged
├── config.rs       unchanged
├── constants.rs    unchanged
├── server.rs       back-compat re-export shim → crate::engine::*
├── tool.rs         back-compat re-export shim → crate::mcp::*
└── types.rs        back-compat re-export shim → crate::domain::*
                                                + crate::mcp::args::*
```

The Tauri viewer's `use deliberate_mcp::types::*` and the integration
tests' `use deliberate_mcp::server::*` keep working through the shims
in `types.rs`/`server.rs`/`tool.rs` — no consumer-side import sweep
needed.

### What this enables

- Clear navigation: when an agent's structuredContent doesn't match,
  open `src/output_schemas/<tool>.rs`. When validation fires
  unexpectedly, open `src/engine/validation.rs`. The 2884-line
  monolith is gone.
- SRP enforcement: each file has one reason to change. Domain types
  don't depend on the engine; output schemas don't depend on the
  MCP adapter.
- Easier extension: adding a new tool means a new file in `mcp/` and
  a new file in `output_schemas/`, not a 200-line surgery on
  `tool.rs` and `output_schemas.rs`.

### Engine decomposition

`src/engine/` is fully split by concern. No file over ~500 lines:

```
src/engine/
├── core.rs        334 lines — struct, ctors, accessors, persistence
├── process.rs     505 lines — process_step + warnings + helpers
├── snapshots.rs   417 lines — read-only JSON aggregations
├── validation.rs  439 lines — required-fields, deps, confidence + recovery
├── numbering.rs   317 lines — project-wide step-number bookkeeping
├── impact.rs      249 lines — dependency-graph walks
├── recovery.rs    197 lines — XML-injection extractors
├── mutations.rs   159 lines — revise/pin/branch-status/clear
├── sessions.rs    141 lines — lifecycle + clock helpers
├── branching.rs   118 lines — branch creation + depth
├── lookup.rs      111 lines — step lookups + search
├── revisions.rs    58 lines — back-pointer bookkeeping
├── export.rs       48 lines — format dispatch
└── mod.rs          29 lines
```

13 cohesive modules, each with one reason to change. Down from the
original 2884-line `server.rs`.

### Backlog

- `tests/server.rs` (2168 lines) is unchanged. Test re-organization
  is a separate undertaking from the source-side reorg.

### Verification

`cargo test`: 191/191 pass. `cargo clippy --all-targets -- -D warnings`:
clean. Wire-level smoke test against the binary: identical behavior
to 0.2.1.

## 0.2.1 — 2026-05-21

### XML-injection recovery actually works now

Empirical scan of 12 Claude Code session logs (~11k turns) found
**24 failed `deliberate_record_step` calls** caused by the agent
serializing sibling parameters inside `thought` as bare XML tags
(`<outcome>...</outcome>`, `<rationale>...</rationale>`, etc.). One
session burned **10 retries in a row**. The existing
`recover_xml_injection` function never fired against any of them
because it only matched `<parameter name="X">VALUE</parameter>`
(Claude Code's literal wire syntax), not the looser `<X>VALUE</X>`
form agents actually produce.

This release adds a second-pattern extractor that catches the bare-tag
form, dispatches the recovered values into every field on
`DeliberateStep` (including arrays like `dependencies`/`tools_used`
and booleans like `pinned`/`is_final_step`), and tolerates unclosed
tags at EOF for the case where the agent's output got truncated
mid-value.

**Regression on the actual failed inputs**: 18/19 (94.7%) now succeed
with a recovery warning instead of erroring. The remaining 1 + 4
non-XML-injection cases are genuine "agent forgot a field" omissions
that aren't recovery-amenable.

The diagnostic message for the unrecoverable cases is also rewritten
to name the specific pattern: "your `thought` text contains
structural tags like `<outcome>...</outcome>` used as if they were
section headers" and to restate that each field is a separate
top-level JSON parameter.

`deliberate_record_step`'s description now includes an explicit
❌/✅ anti-example for the bare-tag pattern.

### Out of scope

The 10-item theoretical punch list from the earlier `/plan`
(enum-ifying `purpose`/`format`/`status`, `min`/`max` on `confidence`,
`anyOf` resolution on `next_action`/`dependencies`, branch_id schema
hints, `session_id` schema honesty, naming normalization) — none of
those failure modes had any empirical incidents in the rikttp scan.
Deferred until evidence shows they're costing retries.

## 0.2.0 — 2026-05-20

### MCP schema modernized to 2026-05 standards

Every tool now ships with the post-2025-06-18 spec surface: `annotations`
(`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`,
`title`), `outputSchema` on JSON-returning tools, and
`structuredContent` on call results so 2026 clients can validate
responses without parsing prose. See the [Mar-2026 official annotations
guidance](https://blog.modelcontextprotocol.io/posts/2026-03-16-tool-annotations/)
for why this matters for auto-approval gating.

### Breaking: tool rename

Tool names are now verb-first and disambiguated, per the
[arXiv:2602.14878 (Feb 2026)](https://arxiv.org/html/2602.14878v1) audit
finding that tool names are the strongest selection signal.

| 0.1 | 0.2 |
|---|---|
| `deliberate` | `deliberate_record_step` |
| `deliberate_status` | `deliberate_engine_status` |
| `deliberate_export` | `deliberate_export_trace` |
| `deliberate_step` | `deliberate_get_step` |
| `deliberate_search` | `deliberate_search_trace` |
| `deliberate_impact` | `deliberate_step_impact` |
| `deliberate_pin` | `deliberate_pin_step` |
| `deliberate_checkpoint` | `deliberate_trace_checkpoint` |
| `deliberate_clear` | `deliberate_wipe_trace` |
| `deliberate_revise_estimate` | *(unchanged)* |
| `deliberate_set_branch_status` | *(unchanged)* |

Update any `CLAUDE.md`, agent prompts, or scripts that hard-code the old
names. There are no backwards-compatibility aliases.

### Other changes

- Server-level `instructions` rewritten as a decision-tree preamble.
- Every tool description follows the same `when-to-use / inputs /
  returns / pitfalls` shape and includes an explicit pitfalls section
  (descriptions with explicit gotchas score highest on agent selection
  accuracy per the arXiv audit).
- Error responses on tools with an `outputSchema` now use
  `structuredContent` with a uniform `{ error_kind, message, hint }`
  envelope so agents can pattern-match `error_kind` instead of parsing
  English.
- 16 new integration tests in `tests/mcp_schema.rs` exercise the
  `tools/list` and `tools/call` surface end-to-end.

### Unchanged

- Engine semantics, persistence format, session handling.
- Tauri viewer wire shape (`DeliberateStep`); only the surrounding
  `CallToolResult` envelope gains `structuredContent`.
- Environment variables.
- Binary name (`deliberate-mcp`).
