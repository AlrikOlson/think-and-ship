use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use super::service::ShipService;
use crate::ship::domain::action::ActionType;
use crate::ship::domain::artifact::{Artifact, ArtifactType};
use crate::ship::domain::check::CheckType;
use crate::ship::domain::task::TaskType;

impl ShipService {
    pub(crate) fn make_tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::tool_router()
    }
}

// ── Arg types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetObjectiveArgs {
    #[serde(default)]
    pub description: String,
    #[serde(default, deserialize_with = "crate::infra::coerce::string_or_seq")]
    pub acceptance_criteria: Vec<String>,
    #[serde(default, deserialize_with = "crate::infra::coerce::string_or_seq")]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlanArgs {
    #[serde(default)]
    pub action: PlanAction,
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub task_type: Option<TaskType>,
    #[serde(default)]
    pub estimate: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub think_branch: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    Add,
    Remove,
    Reorder,
    /// Catches a missing or unrecognized `action` value. Rather than reject
    /// with a JSON-RPC -32602 (which cancels sibling tool calls in a parallel
    /// batch), the value deserializes to this and the handler returns a soft
    /// error naming the valid actions.
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartArgs {
    #[serde(default)]
    pub task_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordArgs {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(rename = "type", default = "default_action_type")]
    pub action_type: ActionType,
    pub description: String,
    #[serde(default, deserialize_with = "crate::infra::coerce::string_or_seq")]
    pub files_touched: Vec<String>,
    #[serde(default, deserialize_with = "crate::infra::coerce::string_or_seq")]
    pub tools_used: Vec<String>,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub think_step: Option<u32>,
}

fn default_action_type() -> ActionType {
    ActionType::Code
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompleteArgs {
    pub task_id: String,
    #[serde(default)]
    pub artifacts: Vec<ArtifactInput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ArtifactInput {
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BlockArgs {
    pub task_id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CheckArgs {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(rename = "type")]
    pub check_type: CheckType,
    pub name: String,
    /// Self-reported pass/fail. Optional when `command` is given (the real exit
    /// code decides). Required otherwise.
    #[serde(default)]
    pub passed: Option<bool>,
    #[serde(default)]
    pub details: String,
    #[serde(default = "default_true")]
    pub required: bool,
    /// A shell command to actually run (e.g. "cargo test"). When present the
    /// server executes it, and `passed`/`exit_code`/`details` are taken from the
    /// real result — the check is `verified` and cannot be faked.
    #[serde(default)]
    pub command: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShipArgs {
    #[serde(default)]
    pub artifacts: Vec<ArtifactInput>,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportArgs {
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "markdown".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GateOpenArgs {
    /// The one plain-language sentence a human answers.
    #[serde(default)]
    pub question: String,
    /// Plain-prose context: what happens on each answer, what was verified.
    #[serde(default)]
    pub body: String,
    /// Array of {key, label} choices. Omitted = yes/no.
    #[serde(default)]
    pub options: serde_json::Value,
    /// The safe answer an unanswered gate resolves to at expiry. REQUIRED.
    #[serde(default, rename = "default")]
    pub default_key: Option<String>,
    /// Seconds until the gate answers itself with the default (default 3600).
    #[serde(default)]
    pub timeout_secs: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GateWaitArgs {
    #[serde(default)]
    pub gate_id: String,
    /// Seconds this ONE call may wait before returning `pending` (default 25,
    /// max 55 — always under the MCP client's own call timeout; loop to keep
    /// waiting).
    #[serde(default)]
    pub wait_secs: Option<i64>,
}

// ── Verified-check command execution ───────────────────────────────

/// Result of running a `ship_check` command.
struct CheckOutcome {
    passed: bool,
    exit_code: Option<i32>,
    output_tail: String,
}

/// Max chars of command output retained in the check `details`.
const CHECK_OUTPUT_TAIL: usize = 4000;
/// A verified check command is killed after this long so a hung gate can't pin
/// a slot forever.
///
/// Public because [`crate::mcp::tasks::TASK_TTL_MS`] is *derived* from it: a
/// task retention window shorter than the longest a gate may run would mark a
/// still-running gate `failed`.
pub const CHECK_TIMEOUT_SECS: u64 = 900;

/// Run a check command in the server's working directory (the project dir),
/// capturing exit code + a tail of combined stdout/stderr. The command runs
/// through the platform shell so `&&`, pipes, and args work as written.
async fn run_check_command(cmd: &str) -> CheckOutcome {
    use std::process::Stdio;
    use tokio::process::Command;

    #[cfg(windows)]
    let (shell, flag) = ("cmd", "/C");
    #[cfg(not(windows))]
    let (shell, flag) = ("sh", "-c");

    let mut command = Command::new(shell);
    command
        .arg(flag)
        .arg(cmd)
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let timeout = std::time::Duration::from_secs(CHECK_TIMEOUT_SECS);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => {
            let code = output.status.code();
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                combined.push_str(&stderr);
            }
            let code_str = code.map_or_else(|| "signal".to_string(), |c| c.to_string());
            CheckOutcome {
                passed: output.status.success(),
                exit_code: code,
                output_tail: format!("$ {cmd}\n[exit {code_str}]\n{}", tail_chars(&combined)),
            }
        }
        Ok(Err(e)) => CheckOutcome {
            passed: false,
            exit_code: None,
            output_tail: format!("failed to run `{cmd}`: {e}"),
        },
        Err(_) => CheckOutcome {
            passed: false,
            exit_code: None,
            output_tail: format!("`{cmd}` timed out after {CHECK_TIMEOUT_SECS}s"),
        },
    }
}

/// Keep the last [`CHECK_OUTPUT_TAIL`] chars of `s` (failures show up at the end
/// of build/test output), prefixed with an elision marker when trimmed.
fn tail_chars(s: &str) -> String {
    let s = s.trim_end();
    let n = s.chars().count();
    if n <= CHECK_OUTPUT_TAIL {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - CHECK_OUTPUT_TAIL).collect();
    format!("…(truncated {} chars)…\n{tail}", n - CHECK_OUTPUT_TAIL)
}

// ── Tool handlers ──────────────────────────────────────────────────

#[tool_router]
impl ShipService {
    #[tool(
        name = "ship_set_objective",
        description = "Define what 'done' means for this development cycle. Sets the goal, acceptance criteria, constraints, and scope. Call this before planning any tasks.\n\nInputs: description (required), acceptance_criteria (string[]), constraints (string[]), scope (string). constraints/acceptance_criteria also accept a single string (coerced to a one-element list).\n\nReturns: the objective as set; plus cleared_stale_tasks + note when a fresh cycle was started.\n\nPitfalls: overwriting an IN-FLIGHT objective preserves its tasks. But if the previous objective was already shipped (completed/abandoned), this starts a FRESH cycle and clears the old tasks — so a new chunk never inherits the prior cycle's explore/implement/verify tasks.",
        annotations(
            title = "Set objective",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn ship_set_objective(
        &self,
        Parameters(args): Parameters<SetObjectiveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let cleared = engine.set_objective(
            args.description,
            args.acceptance_criteria,
            args.constraints,
            args.scope,
        );
        let mut value = serde_json::to_value(engine.objective.as_ref().unwrap()).unwrap();
        if cleared > 0
            && let Some(map) = value.as_object_mut()
        {
            map.insert("cleared_stale_tasks".into(), serde_json::json!(cleared));
            map.insert(
                "note".into(),
                serde_json::json!(format!(
                    "Started a fresh cycle: cleared {cleared} task(s) left over from the previous (finished) objective."
                )),
            );
        }
        Ok(Self::ok_structured(value))
    }

    #[tool(
        name = "ship_plan",
        description = "Add, remove, or reorder tasks in the execution plan.\n\nInputs: action ('add'|'remove'|'reorder'), task_id (required), title (required for add), task_type ('implement'|'test'|'review'|'config'|'docs'|'research'), estimate ('trivial'|'small'|'medium'|'large'), after (task_id to place after, for reorder), think_branch (optional cross-ref to a think_* branch).\n\nReturns: the updated task list.\n\nPitfalls: task_id must be unique within the objective for 'add'. Cannot remove an active or completed task.",
        annotations(
            title = "Plan tasks",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn ship_plan(
        &self,
        Parameters(args): Parameters<PlanArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match args.action {
            PlanAction::Add => {
                let title = args.title.unwrap_or_else(|| args.task_id.clone());
                let task_type = args.task_type.unwrap_or(TaskType::Implement);
                engine.add_task(
                    args.task_id,
                    title,
                    task_type,
                    args.estimate,
                    args.think_branch,
                );
            }
            PlanAction::Remove => {
                if let Err(e) = engine.remove_task(&args.task_id) {
                    return Ok(Self::err_structured("invalid_state", e));
                }
            }
            PlanAction::Reorder => {
                if let Err(e) = engine.reorder_task(&args.task_id, args.after.as_deref()) {
                    return Ok(Self::err_structured("invalid_state", e));
                }
            }
            PlanAction::Unknown => {
                return Ok(Self::err_structured(
                    "invalid_args",
                    "action must be one of: add, remove, reorder",
                ));
            }
        }
        Ok(Self::ok_structured(engine.plan_summary()))
    }

    #[tool(
        name = "ship_start",
        description = "Begin work on a task. Sets its status to active and records the start time. Only one task can be active at a time.\n\nInputs: task_id (required).\n\nReturns: the started task.\n\nPitfalls: fails if another task is already active. Complete or block the current task first.",
        annotations(
            title = "Start task",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn ship_start(
        &self,
        Parameters(args): Parameters<StartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.start_task(&args.task_id) {
            Ok(task) => Ok(Self::ok_structured(serde_json::to_value(task).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "ship_record",
        description = "Log an action within the active task. This is the primary workhorse — call it every time you do something: write code, run a command, make a decision, research something.\n\nInputs: task_id (optional — defaults to active task), type ('code'|'test'|'debug'|'research'|'config'|'refactor'|'review'), description (required), files_touched (string[]), tools_used (string[]), result (string), think_step (optional u32 — cross-ref to the think_* step that motivated this action).\n\nReturns: the recorded action with its assigned id.\n\nPitfalls: if no task is active and no task_id is provided, the call fails.",
        annotations(
            title = "Record action",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn ship_record(
        &self,
        Parameters(args): Parameters<RecordArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.record_action(
            args.task_id.as_deref(),
            args.action_type,
            args.description,
            args.files_touched,
            args.tools_used,
            args.result,
            args.think_step,
        ) {
            Ok(action) => Ok(Self::ok_structured(serde_json::to_value(action).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "ship_complete",
        description = "Close a task and record what was produced.\n\nInputs: task_id (required), artifacts (array of {type, ref, description}).\n\nReturns: the completed task.\n\nPitfalls: can only complete a task that is active or blocked.",
        annotations(
            title = "Complete task",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn ship_complete(
        &self,
        Parameters(args): Parameters<CompleteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let artifacts: Vec<Artifact> = args
            .artifacts
            .into_iter()
            .map(|a| Artifact {
                artifact_type: a.artifact_type,
                reference: a.reference,
                description: a.description,
            })
            .collect();
        match engine.complete_task(&args.task_id, artifacts) {
            Ok(task) => Ok(Self::ok_structured(serde_json::to_value(task).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "ship_block",
        description = "Mark a task as blocked with a reason.\n\nInputs: task_id (required), reason (required).\n\nReturns: the blocked task.\n\nPitfalls: can only block an active task.",
        annotations(
            title = "Block task",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn ship_block(
        &self,
        Parameters(args): Parameters<BlockArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.block_task(&args.task_id, args.reason) {
            Ok(task) => Ok(Self::ok_structured(serde_json::to_value(task).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "ship_check",
        description = "Record a quality gate result — a test run, lint pass, type check, build, code review, or manual verification.\n\nPREFER passing `command`: the server runs it, captures the real exit code, and sets passed/details/exit_code from the result. Such a check is `verified:true` and cannot be faked. Use self-reported `passed` only for gates that can't be expressed as a command (e.g. manual review).\n\nInputs: task_id (optional — defaults to active task), type ('test'|'lint'|'typecheck'|'build'|'review'|'manual'), name (required — e.g. 'cargo test'), command (optional shell command to run and verify), passed (bool — required only when no command is given), details (string), required (bool, default true).\n\nReturns: the recorded check incl. verified + exit_code.\n\nPitfalls: required checks that failed — or that passed but are only self-reported (verified:false) — are flagged when ship_finalize is called.",
        annotations(
            title = "Record check",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn ship_check(
        &self,
        Parameters(args): Parameters<CheckArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Run the command (if any) BEFORE locking the engine so the lock isn't
        // held across a potentially slow subprocess.
        let (passed, details, verified, command, exit_code) = match &args.command {
            Some(cmd) => {
                let outcome = run_check_command(cmd).await;
                let details = match (args.details.is_empty(), &outcome.output_tail) {
                    (true, tail) => tail.clone(),
                    (false, tail) => format!("{}\n{}", args.details, tail),
                };
                (
                    outcome.passed,
                    details,
                    true,
                    Some(cmd.clone()),
                    outcome.exit_code,
                )
            }
            None => match args.passed {
                Some(p) => (p, args.details, false, None, None),
                None => {
                    return Ok(Self::err_structured(
                        "invalid_args",
                        "`passed` is required when no `command` is given (or pass a `command` to run and verify the check).",
                    ));
                }
            },
        };
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.record_check_full(
            args.task_id.as_deref(),
            args.check_type,
            args.name,
            passed,
            details,
            args.required,
            verified,
            command,
            exit_code,
        ) {
            Ok(check) => Ok(Self::ok_structured(serde_json::to_value(check).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "ship_finalize",
        description = "Mark the objective as completed and record final artifacts (commits, PRs, deployments). Reviews all checks and warns about any required checks that failed or are missing.\n\nInputs: artifacts (array of {type, ref, description}), summary (optional string).\n\nReturns: ship report — objective status, task completion stats, check summary, warnings about failed/missing required checks.\n\nPitfalls: does NOT block on failed checks — it warns. The trace records whether the agent shipped with failures.",
        annotations(
            title = "Ship objective",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn ship_finalize(
        &self,
        Parameters(args): Parameters<ShipArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let artifacts: Vec<Artifact> = args
            .artifacts
            .into_iter()
            .map(|a| Artifact {
                artifact_type: a.artifact_type,
                reference: a.reference,
                description: a.description,
            })
            .collect();
        let report = engine.ship(artifacts, args.summary);
        Ok(Self::ok_structured(report))
    }

    #[tool(
        name = "ship_gate_open",
        description = "Open an approval gate: pause-worthy work that needs a human yes, answered from the workspace webapp. Creates a ship/gate record in the cloud workspace and returns immediately with the gate id — then poll ship_gate_wait. HEADLESS-SAFE BY CONSTRUCTION: `default` is required, the gate answers itself with that default at `timeout_secs`, and with no cloud workspace connected the call resolves to the default at once instead of opening anything.\n\nInputs: question (required — one plain sentence), body (plain-prose context), options (array of {key,label}; omitted = yes/no), default (required — the option key an unanswered gate resolves to), timeout_secs (default 3600, clamped 30..604800).\n\nReturns: {opened:true, gate_id, expires_at, ...} or {opened:false, resolved:<default>, reason} when no workspace could carry the gate.\n\nPitfalls: pick the SAFE default (usually 'no'/'hold') — it is what headless expiry applies.",
        annotations(
            title = "Open approval gate",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        )
    )]
    pub async fn ship_gate_open(
        &self,
        Parameters(args): Parameters<GateOpenArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        use crate::ship::gate::{self, GateOption};
        // Snapshot what we need and DROP the lock before any network await.
        let (client, tenant, task_id) = {
            let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
            (
                engine.cloud_client(),
                engine.project_id.clone(),
                engine.active_task_wire_id(),
            )
        };
        let options: Vec<GateOption> = if args.options.is_null() {
            vec![
                GateOption {
                    key: "yes".into(),
                    label: "Yes, go ahead".into(),
                },
                GateOption {
                    key: "no".into(),
                    label: "No, hold".into(),
                },
            ]
        } else {
            match serde_json::from_value(args.options) {
                Ok(o) => o,
                Err(e) => {
                    return Ok(Self::err_structured(
                        "invalid_args",
                        format!("`options` must be an array of {{key, label}} objects: {e}"),
                    ));
                }
            }
        };
        let Some(default_key) = args.default_key.filter(|d| !d.trim().is_empty()) else {
            return Ok(Self::err_structured(
                "invalid_args",
                "`default` is required — the option key an unanswered gate resolves to at expiry. \
                 Pick the SAFE answer.",
            ));
        };
        let gate = match gate::open(
            uuid::Uuid::new_v4().to_string(),
            &args.question,
            &args.body,
            options,
            &default_key,
            args.timeout_secs.unwrap_or(gate::DEFAULT_TIMEOUT_SECS),
            chrono::Utc::now(),
        ) {
            Ok(g) => g,
            Err(e) => return Ok(Self::err_structured("invalid_args", e)),
        };
        let Some(client) = client else {
            return Ok(Self::ok_structured(serde_json::json!({
                "opened": false,
                "state": "unopened",
                "resolved": gate.default_key,
                "reason": "no cloud workspace is connected, so nobody could answer — the declared default applies",
            })));
        };
        let envelope = crate::cloud::build::from_gate(&tenant, &gate, task_id.as_deref());
        match client.push(&envelope).await {
            Ok(_) => Ok(Self::ok_structured(serde_json::json!({
                "opened": true,
                "state": "pending",
                "gate_id": gate.id,
                "question": gate.question,
                "default": gate.default_key,
                "expires_at": gate.expires_at,
                "hint": "poll ship_gate_wait(gate_id) until it returns answered or expired; an unanswered gate resolves to the default at expires_at",
            }))),
            Err(e) => Ok(Self::ok_structured(serde_json::json!({
                "opened": false,
                "state": "unopened",
                "resolved": gate.default_key,
                "reason": format!(
                    "the workspace could not be reached ({e}) — nobody could answer, so the declared default applies"
                ),
            }))),
        }
    }

    #[tool(
        name = "ship_gate_wait",
        description = "Check (and briefly wait on) an open approval gate. Polls the cloud workspace; returns as soon as the gate is answered or expired, else after wait_secs with state 'pending' — loop on pending to keep waiting. The wait is BOUNDED (max 55s per call, under any MCP client timeout); an unanswered gate deterministically resolves to its declared default once expires_at passes, so a headless session always completes.\n\nInputs: gate_id (required — from ship_gate_open), wait_secs (default 25, clamped 0..55).\n\nReturns: {state:'answered', choice, decided_by, decided_at, note?} | {state:'expired', choice:<default>} | {state:'pending', seconds_left}.\n\nPitfalls: if the workspace is unreachable, this returns a soft error — apply your declared default yourself once expires_at passes.",
        annotations(
            title = "Wait on approval gate",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true,
        )
    )]
    pub async fn ship_gate_wait(
        &self,
        Parameters(args): Parameters<GateWaitArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        use crate::ship::gate::{self, Resolution};
        if args.gate_id.trim().is_empty() {
            return Ok(Self::err_structured(
                "invalid_args",
                "`gate_id` is required (returned by ship_gate_open)",
            ));
        }
        let client = {
            let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
            engine.cloud_client()
        };
        let Some(client) = client else {
            return Ok(Self::err_structured(
                "no_cloud",
                "no cloud workspace is connected — gates live in the workspace. Apply your declared default.",
            ));
        };
        let wait_secs = args.wait_secs.unwrap_or(25).clamp(0, 55);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs as u64);
        loop {
            let envelope = match client.get("ship", "gate", &args.gate_id).await {
                Ok(Some(v)) => v,
                Ok(None) => {
                    return Ok(Self::err_structured(
                        "not_found",
                        format!("gate '{}' was not found in the workspace", args.gate_id),
                    ));
                }
                Err(e) => {
                    return Ok(Self::err_structured(
                        "cloud_unreachable",
                        format!(
                            "the workspace could not be reached ({e}) — apply your declared default once expires_at passes"
                        ),
                    ));
                }
            };
            let record = envelope
                .get("record")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            match gate::resolve(&record, chrono::Utc::now()) {
                Ok(Resolution::Answered(a)) => {
                    return Ok(Self::ok_structured(serde_json::json!({
                        "state": "answered",
                        "choice": a.choice,
                        "decided_by": a.decided_by,
                        "decided_at": a.decided_at,
                        "note": a.note,
                    })));
                }
                Ok(Resolution::Expired { choice }) => {
                    return Ok(Self::ok_structured(serde_json::json!({
                        "state": "expired",
                        "choice": choice,
                        "note": "nobody answered before expires_at — the declared default applies",
                    })));
                }
                Ok(Resolution::Pending { seconds_left }) => {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        return Ok(Self::ok_structured(serde_json::json!({
                            "state": "pending",
                            "seconds_left": seconds_left,
                            "hint": "call ship_gate_wait again to keep waiting; the gate resolves to its default at expires_at",
                        })));
                    }
                    tokio::time::sleep(remaining.min(std::time::Duration::from_secs(3))).await;
                }
                Err(e) => return Ok(Self::err_structured("malformed_gate", e)),
            }
        }
    }

    #[tool(
        name = "ship_status",
        description = "Full state snapshot of the current execution cycle. Call this after context compaction to reconstruct where you are — objective, plan progress, active task, recent actions, pending checks, produced artifacts, and think_* cross-references.\n\nInputs: none.\n\nReturns: complete state including objective, tasks with status counts, active task details, recent actions, all checks, all artifacts.\n\nPitfalls: none — this is the recovery tool. Call it whenever you're unsure of the current state.",
        annotations(
            title = "Get status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn ship_status(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        Ok(Self::ok_structured(engine.status()))
    }

    #[tool(
        name = "ship_export",
        description = "Export the full execution trace.\n\nInputs: format ('markdown'|'json', default 'markdown').\n\nReturns: the formatted trace.\n\nPitfalls: none.",
        annotations(
            title = "Export trace",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn ship_export(
        &self,
        Parameters(args): Parameters<ExportArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let output = engine.export(&args.format);
        Ok(Self::ok_structured(
            serde_json::json!({ "format": args.format, "trace": output }),
        ))
    }

    #[tool(
        name = "ship_reset",
        description = "Wipe all execution state — objective, tasks, actions, checks, artifacts. This is destructive and irreversible.\n\nInputs: none.\n\nReturns: confirmation.\n\nPitfalls: there is no undo.",
        annotations(
            title = "Reset (destructive)",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn ship_reset(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        engine.reset();
        Ok(Self::ok_structured(
            serde_json::json!({ "status": "cleared" }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_runner_captures_a_real_pass() {
        let out = run_check_command("exit 0").await;
        assert!(out.passed);
        assert_eq!(out.exit_code, Some(0));
    }

    #[tokio::test]
    async fn command_runner_captures_a_real_failure() {
        // A green check can't be faked: a failing command yields passed:false
        // and the real exit code, no matter what the agent claims. The fixture
        // speaks the platform shell, like run_check_command itself: `;` is not
        // a separator under `cmd /C`.
        #[cfg(windows)]
        let cmd = "echo boom 1>&2 & exit 7";
        #[cfg(not(windows))]
        let cmd = "echo boom >&2; exit 7";
        let out = run_check_command(cmd).await;
        assert!(!out.passed);
        assert_eq!(out.exit_code, Some(7));
        assert!(out.output_tail.contains("boom"));
    }

    #[test]
    fn tail_chars_keeps_the_end_and_marks_elision() {
        let s: String = std::iter::repeat_n('a', CHECK_OUTPUT_TAIL + 50).collect();
        let tailed = tail_chars(&s);
        assert!(tailed.contains("truncated"));
        assert!(tailed.ends_with('a'));
    }
}
