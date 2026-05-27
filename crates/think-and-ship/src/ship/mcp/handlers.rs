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
    pub description: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlanArgs {
    pub action: PlanAction,
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
    pub deliberate_branch: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    Add,
    Remove,
    Reorder,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartArgs {
    pub task_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordArgs {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(rename = "type", default = "default_action_type")]
    pub action_type: ActionType,
    pub description: String,
    #[serde(default)]
    pub files_touched: Vec<String>,
    #[serde(default)]
    pub tools_used: Vec<String>,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub deliberate_step: Option<u32>,
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
    pub passed: bool,
    #[serde(default)]
    pub details: String,
    #[serde(default = "default_true")]
    pub required: bool,
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

// ── Tool handlers ──────────────────────────────────────────────────

#[tool_router]
impl ShipService {
    #[tool(
        name = "ship_set_objective",
        description = "Define what 'done' means for this development cycle. Sets the goal, acceptance criteria, constraints, and scope. Call this before planning any tasks.\n\nInputs: description (required), acceptance_criteria (string[]), constraints (string[]), scope (string).\n\nReturns: the objective as set.\n\nPitfalls: calling this again overwrites the current objective. All existing tasks are preserved.",
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
        engine.set_objective(
            args.description,
            args.acceptance_criteria,
            args.constraints,
            args.scope,
        );
        let obj = engine.objective.as_ref().unwrap();
        Ok(Self::ok_structured(serde_json::to_value(obj).unwrap()))
    }

    #[tool(
        name = "ship_plan",
        description = "Add, remove, or reorder tasks in the execution plan.\n\nInputs: action ('add'|'remove'|'reorder'), task_id (required), title (required for add), task_type ('implement'|'test'|'review'|'config'|'docs'|'research'), estimate ('trivial'|'small'|'medium'|'large'), after (task_id to place after, for reorder), deliberate_branch (optional cross-ref to deliberate-mcp branch).\n\nReturns: the updated task list.\n\nPitfalls: task_id must be unique within the objective for 'add'. Cannot remove an active or completed task.",
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
                    args.deliberate_branch,
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
        description = "Log an action within the active task. This is the primary workhorse — call it every time you do something: write code, run a command, make a decision, research something.\n\nInputs: task_id (optional — defaults to active task), type ('code'|'test'|'debug'|'research'|'config'|'refactor'|'review'), description (required), files_touched (string[]), tools_used (string[]), result (string), deliberate_step (optional u32 — cross-reference to the deliberate-mcp step that motivated this action).\n\nReturns: the recorded action with its assigned id.\n\nPitfalls: if no task is active and no task_id is provided, the call fails.",
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
            args.deliberate_step,
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
        description = "Record a quality gate result — a test run, lint pass, type check, build, code review, or manual verification.\n\nInputs: task_id (optional — defaults to active task), type ('test'|'lint'|'typecheck'|'build'|'review'|'manual'), name (required — e.g. 'cargo test', 'npm run lint'), passed (bool, required), details (string — output summary or failure reason), required (bool, default true — whether this gate should block shipping).\n\nReturns: the recorded check.\n\nPitfalls: checks with required=true and passed=false will generate a warning when ship_finalize is called.",
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
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.record_check(
            args.task_id.as_deref(),
            args.check_type,
            args.name,
            args.passed,
            args.details,
            args.required,
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
        name = "ship_status",
        description = "Full state snapshot of the current execution cycle. Call this after context compaction to reconstruct where you are — objective, plan progress, active task, recent actions, pending checks, produced artifacts, and deliberate-mcp cross-references.\n\nInputs: none.\n\nReturns: complete state including objective, tasks with status counts, active task details, recent actions, all checks, all artifacts.\n\nPitfalls: none — this is the recovery tool. Call it whenever you're unsure of the current state.",
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
