//! The `instructions` field returned in the MCP `initialize` response.
//!
//! Some clients surface this in the model's system prompt; others log it
//! and move on. Either way it should give a model that has never seen
//! deliberate-mcp before a tight orientation that prevents the most
//! common misuse patterns.

pub const SERVER_INSTRUCTIONS: &str = r#"deliberate-mcp records structured, branching, revisable reasoning.

When to call which tool:
  - Recording a new thought             → deliberate_record_step
  - Adjusting the total estimate         → deliberate_revise_estimate
  - Pinning a load-bearing conclusion    → deliberate_pin_step
  - Marking a branch active/merged/dead  → deliberate_set_branch_status
  - Checking trace-wide health           → deliberate_trace_checkpoint
  - Fetching a specific step             → deliberate_get_step
  - Searching across the trace           → deliberate_search_trace
  - Computing blast radius of revision   → deliberate_step_impact
  - Engine introspection                 → deliberate_engine_status
  - Exporting the trace                  → deliberate_export_trace
  - Wiping everything (destructive)      → deliberate_wipe_trace

Step numbers are project-global; never reuse. Pin the original problem
statement so later steps keep it in view. Every JSON-returning tool
advertises an outputSchema and emits structuredContent — prefer parsing
that over the text content.

When using resolute-mcp alongside this server, pass `execution_ref` on
deliberate_record_step to link reasoning to execution (e.g.
"task:auth-refactor", "action:42"). Search finds execution_ref values."#;
