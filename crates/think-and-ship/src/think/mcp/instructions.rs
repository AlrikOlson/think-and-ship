//! The `instructions` field returned in the MCP `initialize` response.
//!
//! Some clients surface this in the model's system prompt; others log it
//! and move on. Either way it should give a model that has never seen
//! deliberate-mcp before a tight orientation that prevents the most
//! common misuse patterns.

pub const SERVER_INSTRUCTIONS: &str = r#"deliberate-mcp records structured, branching, revisable reasoning.

When to call which tool:
  - Recording a new thought             → think_record_step
  - Adjusting the total estimate         → think_revise_estimate
  - Pinning a load-bearing conclusion    → think_pin_step
  - Marking a branch active/merged/dead  → think_set_branch_status
  - Checking trace-wide health           → think_trace_checkpoint
  - Fetching a specific step             → think_get_step
  - Searching across the trace           → think_search_trace
  - Computing blast radius of revision   → think_step_impact
  - Engine introspection                 → think_engine_status
  - Exporting the trace                  → think_export_trace
  - Wiping everything (destructive)      → think_wipe_trace

Step numbers are project-global; never reuse. Pin the original problem
statement so later steps keep it in view. Every JSON-returning tool
advertises an outputSchema and emits structuredContent — prefer parsing
that over the text content.

When using resolute-mcp alongside this server, pass `execution_ref` on
think_record_step to link reasoning to execution (e.g.
"task:auth-refactor", "action:42"). Search finds execution_ref values."#;
