//! SEP-2663 Tasks: a long gate stops holding the tool call open.
//!
//! `ship_check` with a `command` is the one tool in this server that provably
//! blocks for minutes: running `cargo test --workspace` inside a single
//! `tools/call` is how this project's own gates are recorded, and it is ~90s
//! cold. Every other tool here returns in single-digit milliseconds. Tasks are
//! the protocol's answer: the server materializes a handle, returns it
//! immediately, and the client polls `tasks/get` for the real result.
//!
//! # Composition with progress (`crate::mcp::progress`)
//!
//! [`crate::mcp::progress`] wrote the division down before this module existed:
//! **tasks change how long the call stays open; progress changes what the human
//! sees while it is open.** The rule it set — *no second progress mechanism* —
//! is obeyed structurally rather than by review. A task-eligible call `return`s
//! from `crate::mcp::unified::UnifiedService::call_tool` **before** that
//! function's own `Heartbeat::start`, and the one heartbeat is started inside
//! the spawned future instead, where [`rmcp::task_manager::TaskContext`] finally
//! knows the task id. So exactly one ticker exists per call, it is the same
//! type emitting the same `notifications/progress` against the same caller
//! `progressToken`, and — as the composition section asked — its tick text names
//! the task.
//!
//! Nothing hangs off `tasks/update`. Doubling every notification was the one
//! failure mode that section named, and the only way to get it is a second
//! emitter.
//!
//! # The server decides, and the client is never asked
//!
//! There is no `as_task` parameter and no schema field. Adding one would spend
//! the `tools/list` byte budget (`tools_list_payload_stays_within_budget`) to
//! ask a question the server can answer better: [`wants_task`] knows which call
//! is long, and [`Eligibility::decide`] knows whether the client can handle a
//! handle. A caller that never heard of tasks gets byte-identical blocking
//! behaviour — this is additive, not a migration.
//!
//! # Why [`Eligibility`] is a type and not a boolean
//!
//! SEP-2663 forbids returning a `CreateTaskResult` to a client that did not
//! declare the tasks extension; rmcp enforces it with an inline guard in its
//! `CallToolRequest` arm, which turns the mistake into a wire error the caller
//! sees. A `bool` we remember to check is exactly the kind of guard that is
//! weaker than one the type system enforces. So [`Eligibility`] has no public
//! constructor: the only way to obtain one is [`Eligibility::decide`], which
//! returns `None` unless the client declared tasks — and it is consumed by
//! [`Eligibility::spawn`], the only function in this crate that can produce
//! [`CallToolResponse::Task`]. Returning a task to a client that cannot parse
//! one is *unspellable* here, not merely discouraged.
//!
//! # Cancellation is cooperative, and the kill is already in the tree
//!
//! `TaskManager::cancel_task` only flips a watch channel; it never aborts the
//! join handle. The operation has to notice. [`Eligibility::spawn`] therefore
//! races the inner call against [`rmcp::task_manager::TaskContext::cancelled`]
//! in a `select!`, and when cancellation wins, the inner future is **dropped**.
//! `run_check_command` already builds its child with `.kill_on_drop(true)`, so
//! that drop is what kills the actual `cargo test` process — the two facts
//! compose, and no process-killing code is written here. The task then settles
//! terminal `cancelled` via [`TaskExit::Cancelled`], which is neither "passed"
//! nor silently missing.
//!
//! # Degrade, never fail
//!
//! Four situations, one behaviour — the call runs inline exactly as it did
//! before this module existed:
//!
//! 1. The client declared no tasks extension.
//! 2. The tool is not one [`wants_task`] considers long.
//! 3. `ship_check` was called without a `command` (a self-reported check does
//!    no work and returns instantly).
//! 4. The inner call produced something other than
//!    [`CallToolResponse::Complete`] — the task settles `failed` rather than
//!    inventing a shape the client cannot read.

use std::future::Future;

use rmcp::{
    RoleServer,
    model::{CallToolResponse, CreateTaskResult, JsonObject},
    service::RequestContext,
    task_manager::{TaskExit, TaskManager, TaskOptions},
};

use super::progress::Heartbeat;

/// The one tool whose `tools/call` is worth converting into a task.
pub const LONG_GATE_TOOL: &str = "ship_check";

/// The argument whose presence makes [`LONG_GATE_TOOL`] actually do work.
pub const LONG_GATE_ARG: &str = "command";

/// Suggested `tasks/get` polling interval, in milliseconds.
///
/// Deliberately equal to [`super::progress::TICK_INTERVAL`]: a client polling on
/// the same cadence it is already being ticked at learns nothing new between
/// polls, and a human watching sees one rhythm rather than two.
pub const TASK_POLL_INTERVAL_MS: u64 = 2_000;

/// Retention/deadline for a gate task, in milliseconds.
///
/// **Derived, not chosen.** A non-terminal task whose TTL elapses is marked
/// `failed` by rmcp's opportunistic sweep, so a TTL below the longest a gate is
/// allowed to run would kill legitimate work. `run_check_command` caps a gate at
/// `CHECK_TIMEOUT_SECS`, so the TTL is that plus a minute of slack. Deriving it
/// means raising the command timeout cannot silently orphan this constant.
pub const TASK_TTL_MS: u64 = (crate::ship::mcp::CHECK_TIMEOUT_SECS + 60) * 1_000;

/// Does this call do enough work to be worth a task handle?
///
/// Free function, pure, and assertable without a live session — the whole
/// task-vs-inline policy is this one predicate, so a test can pin it by exact
/// value instead of inferring it from wire behaviour.
#[must_use]
pub fn wants_task(tool: &str, arguments: Option<&JsonObject>) -> bool {
    tool == LONG_GATE_TOOL
        && arguments.is_some_and(|args| {
            args.get(LONG_GATE_ARG)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|cmd| !cmd.trim().is_empty())
        })
}

/// Proof that this call may be answered with a task handle.
///
/// Unconstructible except through [`Eligibility::decide`], and consumed by
/// [`Eligibility::spawn`]. See the module docs for why this is a type.
pub struct Eligibility {
    manager: TaskManager,
    status_message: String,
}

impl Eligibility {
    /// `Some` only when the tool is long-running *and* the client declared the
    /// tasks extension. Both halves are required; neither is inferable from the
    /// other.
    ///
    /// `client_capabilities()` is the same predicate rmcp's own guard uses, so a
    /// `Some` here is exactly a call rmcp will let return a task.
    #[must_use]
    pub fn decide(
        manager: &TaskManager,
        context: &RequestContext<RoleServer>,
        tool: &str,
        arguments: Option<&JsonObject>,
    ) -> Option<Self> {
        if !wants_task(tool, arguments) {
            return None;
        }
        // PERMANENT operator observability, not debug scaffolding. Whether a
        // client declares this extension is invisible from inside the server
        // otherwise — the host's own MCP log records what the client saw of US,
        // never what it declared about itself. Fires only on a long gate, which
        // is a handful of calls per session, and it is the difference between
        // "tasks are working" and "tasks ship correct and entirely dormant".
        // Same standing pattern as `elicit.rs`'s capability line.
        let declared = client_declared_tasks(context);
        eprintln!(
            "think-and-ship: {tool} is a long gate; client {} the tasks extension \
             ({}) — SEP-2663",
            if declared {
                "DECLARED"
            } else {
                "did NOT declare"
            },
            if declared {
                "running as a task"
            } else {
                "blocking inline, as before"
            },
        );
        if !declared {
            return None;
        }
        Some(Self {
            manager: manager.clone(),
            status_message: format!("{tool} running"),
        })
    }

    /// Spawn `inner` as a task and return the handle the client polls.
    ///
    /// `inner` is the *identical* call the blocking path makes — this function
    /// re-implements nothing about running a command, deriving `verified`, or
    /// recording a check. That is what makes "a task check and an inline check
    /// agree" true by construction rather than by review: there is only one
    /// implementation, and both paths await it.
    #[must_use]
    pub fn spawn<F, Fut>(self, heartbeat: HeartbeatSeed, inner: F) -> CallToolResponse
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<CallToolResponse, rmcp::ErrorData>> + Send,
    {
        let options = TaskOptions::new()
            .with_ttl_ms(TASK_TTL_MS)
            .with_poll_interval_ms(TASK_POLL_INTERVAL_MS)
            .with_status_message(self.status_message);
        let task = self.manager.spawn(options, move |ctx| {
            Box::pin(async move {
                // THE one heartbeat for this call, started here rather than at
                // the unified seam because only now does the task id exist.
                // Held for the task's whole lifetime; aborted on drop.
                let _heartbeat = heartbeat.start(ctx.task_id());
                tokio::select! {
                    // Cancellation is cooperative: rmcp only flips a watch
                    // channel. Losing this arm means `tasks/cancel` is accepted
                    // and ignored, and the child keeps burning CPU.
                    () = ctx.cancelled() => Err(TaskExit::Cancelled),
                    response = inner() => match response {
                        Ok(CallToolResponse::Complete(result)) => Ok(result),
                        Ok(_) => Err(TaskExit::Error(rmcp::ErrorData::internal_error(
                            "a task-backed tool returned a non-complete response",
                            None,
                        ))),
                        Err(e) => Err(TaskExit::Error(e)),
                    },
                }
            })
        });
        CallToolResponse::Task(CreateTaskResult::new(task))
    }
}

/// Everything needed to start the call's heartbeat, carried into the spawned
/// future so the ticker can name the task id.
///
/// A `Peer` plus the caller's `progressToken`, deliberately *not* a started
/// [`Heartbeat`]: starting one at the seam and a second one here is the exact
/// doubling `progress.rs` forbids, and a seed cannot tick.
#[derive(Debug)]
pub struct HeartbeatSeed {
    peer: rmcp::Peer<RoleServer>,
    token: Option<rmcp::model::ProgressToken>,
    tool: String,
}

impl HeartbeatSeed {
    #[must_use]
    pub fn new(
        peer: rmcp::Peer<RoleServer>,
        meta: &rmcp::model::RequestMetaObject,
        tool: &str,
    ) -> Self {
        Self {
            peer,
            token: super::progress::token_of(meta),
            tool: tool.to_string(),
        }
    }

    /// The label a task-backed call ticks under. Names the task id so a human
    /// reading `notifications/progress` can tie a tick to the handle they were
    /// handed — the composition section's ask, satisfiable only from inside the
    /// spawned future.
    #[must_use]
    pub fn tick_label(tool: &str, task_id: &str) -> String {
        format!("{tool} (task {task_id})")
    }

    /// Consume the seed into the call's single running heartbeat. Inert when the
    /// caller sent no `progressToken` — degradation 1 of `progress.rs`,
    /// unchanged by tasks.
    #[must_use]
    fn start(self, task_id: &str) -> Heartbeat {
        match self.token {
            Some(token) => Heartbeat::start_with_token(
                self.peer,
                token,
                &Self::tick_label(&self.tool, task_id),
            ),
            None => Heartbeat::inert(),
        }
    }
}

/// Did the client declare the SEP-2663 tasks extension for this request?
///
/// Reads per-request `_meta` capabilities first and the `initialize` handshake
/// second — `RequestContext::client_capabilities` already encodes that
/// precedence, and re-deriving it here would be a second answer to drift from.
#[must_use]
pub fn client_declared_tasks(context: &RequestContext<RoleServer>) -> bool {
    context
        .client_capabilities()
        .is_some_and(|caps| caps.supports_tasks())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(json: serde_json::Value) -> JsonObject {
        json.as_object().expect("object literal").clone()
    }

    #[test]
    fn a_ship_check_with_a_command_is_the_long_gate() {
        assert!(wants_task(
            "ship_check",
            Some(&args(
                serde_json::json!({"name": "t", "command": "cargo test"})
            ))
        ));
    }

    #[test]
    fn a_self_reported_ship_check_stays_inline() {
        // No command means no subprocess: the call returns in microseconds and a
        // task handle would be pure overhead for the client to poll.
        assert!(!wants_task(
            "ship_check",
            Some(&args(serde_json::json!({"name": "t", "passed": true})))
        ));
    }

    #[test]
    fn a_blank_command_stays_inline() {
        // `command: "  "` reaches `run_check_command` and returns immediately.
        // Without the trim this would materialize a task that settles before the
        // client's first poll.
        assert!(!wants_task(
            "ship_check",
            Some(&args(serde_json::json!({"command": "   "})))
        ));
        assert!(!wants_task(
            "ship_check",
            Some(&args(serde_json::json!({"command": ""})))
        ));
    }

    #[test]
    fn a_non_string_command_stays_inline() {
        assert!(!wants_task(
            "ship_check",
            Some(&args(serde_json::json!({"command": 7})))
        ));
    }

    #[test]
    fn no_arguments_at_all_stays_inline() {
        assert!(!wants_task("ship_check", None));
    }

    /// The other 47 tools return in single-digit milliseconds. If this ever
    /// passes, every trivial call becomes a two-round-trip poll.
    #[test]
    fn every_other_tool_stays_inline_even_carrying_a_command_key() {
        for tool in [
            "think_record_step",
            "roadmap_status",
            "signal_capture",
            "tracker_setup",
            "ship_finalize",
        ] {
            assert!(
                !wants_task(
                    tool,
                    Some(&args(serde_json::json!({"command": "cargo test"})))
                ),
                "{tool} must not become a task"
            );
        }
    }

    /// The TTL is not a taste choice: a task marked `failed` while its command
    /// is still legitimately running would report a gate that never finished as
    /// a failure. Asserting the ORDER, not the literal, is what survives someone
    /// raising the command timeout.
    #[test]
    fn the_task_ttl_outlives_the_longest_a_gate_may_run() {
        let gate_ceiling_ms = crate::ship::mcp::CHECK_TIMEOUT_SECS * 1_000;
        assert!(
            TASK_TTL_MS > gate_ceiling_ms,
            "TTL {TASK_TTL_MS}ms must exceed the {gate_ceiling_ms}ms command timeout"
        );
    }

    #[test]
    fn the_poll_interval_matches_the_progress_tick_interval() {
        assert_eq!(
            TASK_POLL_INTERVAL_MS,
            super::super::progress::TICK_INTERVAL.as_millis() as u64
        );
    }

    #[test]
    fn the_tick_label_names_both_the_tool_and_the_task() {
        assert_eq!(
            HeartbeatSeed::tick_label("ship_check", "abc-123"),
            "ship_check (task abc-123)"
        );
        // And it flows through the real message builder unchanged, so a tick a
        // human sees carries the id rather than just the tool.
        assert_eq!(
            super::super::progress::tick_message(
                &HeartbeatSeed::tick_label("ship_check", "abc-123"),
                1
            ),
            "ship_check (task abc-123) still running (2s elapsed)"
        );
    }
}
