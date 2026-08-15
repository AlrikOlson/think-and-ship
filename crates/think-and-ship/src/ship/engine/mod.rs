use chrono::Utc;

use crate::cloud::client::CloudClient;
use crate::ship::broadcast::{BroadcastFrame, Broadcaster};
use crate::ship::domain::action::{Action, ActionType};
use crate::ship::domain::artifact::Artifact;
use crate::ship::domain::check::{Check, CheckType};
use crate::ship::domain::objective::{Objective, ObjectiveStatus};
use crate::ship::domain::task::{Task, TaskStatus, TaskType};
use crate::ship::persistence::Persistence;

pub struct ShipEngine {
    pub objective: Option<Objective>,
    pub tasks: Vec<Task>,
    pub project_id: String,
    next_action_id: u32,
    persistence: Option<Persistence>,
    broadcaster: Option<Broadcaster>,
    /// Optional git-native trace mirror. When set, every mutation is
    /// mirrored into `.think-and-ship/` as an Agent Trace JSONL record and the
    /// session is committed on `ship_finalize`. All git/IO runs on the worker's
    /// own thread, never under this engine's lock. `None` = the default Local
    /// behaviour. Writes are fire-and-forget — a sink error never fails a tool.
    mirror: Option<crate::infra::MirrorWorker>,
    /// Whether mirrored records are `shared` (committed `sessions/`) vs `local`
    /// (gitignored). Default `false`. Only meaningful with `mirror`.
    repo_shared: bool,
    /// Optional cloud sync client. When set, an `ActionRecorded`
    /// mutation fire-and-forget pushes the action envelope to the cloud backend.
    /// `None` (default) = no cloud sync.
    cloud: Option<CloudClient>,
}

impl ShipEngine {
    pub fn new(project_id: String) -> Self {
        Self {
            objective: None,
            tasks: Vec::new(),
            project_id,
            next_action_id: 1,
            persistence: None,
            broadcaster: None,
            mirror: None,
            repo_shared: false,
            cloud: None,
        }
    }

    pub fn with_broadcaster(mut self, broadcaster: Broadcaster) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Attach a cloud client so an `ActionRecorded` mutation fire-and-forget
    /// pushes the action envelope to the cloud backend. Wired by
    /// `cli::build_unified`.
    pub fn with_cloud(mut self, client: CloudClient) -> Self {
        self.cloud = Some(client);
        self
    }

    /// A clone of the attached cloud client, if any. The gate tools take this
    /// and drop the engine lock BEFORE any network await — a gate poll must
    /// never hold the engine hostage.
    #[must_use]
    pub fn cloud_client(&self) -> Option<CloudClient> {
        self.cloud.clone()
    }

    /// The id of the currently active task, if any — the gate's `part_of` edge.
    #[must_use]
    pub fn active_task_id(&self) -> Option<String> {
        self.tasks
            .iter()
            .find(|t| t.status == TaskStatus::Active)
            .map(|t| t.id.clone())
    }

    /// The active task's cycle-scoped wire id (`<cycle>.<task>`), the form a
    /// check's `verifies` edge already uses. Local task ids like `verify`
    /// recur across hundreds of cycles, so an edge carrying one can never be
    /// resolved back to the work it paused; the wire id can. None without an
    /// objective `created_at` — a bare local id would be an unresolvable ref,
    /// so no edge at all is the honest fallback.
    #[must_use]
    pub fn active_task_wire_id(&self) -> Option<String> {
        let local = self.active_task_id()?;
        let created = self.objective.as_ref().and_then(|o| o.created_at.clone())?;
        let cycle = crate::cloud::build::cycle_key(&created);
        Some(format!("{cycle}.{local}"))
    }

    /// Attach a git-native trace sink so mutations are mirrored into the repo's
    /// `.think-and-ship/` as Agent Trace JSONL. `shared` selects the committed
    /// `sessions/` partition (`true`) vs the gitignored `local/` partition
    /// (`false`). Wired by `cli::build_unified`.
    pub fn with_repo_sink(mut self, sink: crate::infra::RepoSink, shared: bool) -> Self {
        self.mirror = Some(crate::infra::MirrorWorker::spawn(sink));
        self.repo_shared = shared;
        self
    }

    /// Block until the git-native mirror has drained every mutation submitted so
    /// far. No-op without a mirror. For graceful shutdown and deterministic tests.
    pub fn flush_mirror(&self) {
        if let Some(m) = &self.mirror {
            m.flush();
        }
    }

    fn broadcast(&self, frame: BroadcastFrame) {
        // Mirror into the git-native trace first, push to the cloud,
        // then fan out to the socket. All fire-and-forget.
        self.mirror_frame_to_repo(&frame);
        self.cloud_push_frame(&frame);
        if let Some(b) = &self.broadcaster {
            b.emit(frame);
        }
    }

    /// Fire-and-forget cloud push of a ship mutation frame (sync-ship-full —
    /// all four kinds). Identity is cycle-scoped (`cloud::build::cycle_key`
    /// from the objective's `created_at`), so wire ids never collide across
    /// cycles. Store-aware: id-only task frames push the FULL current task
    /// state; objective frames push the current objective (re-push on
    /// `ObjectiveShipped` updates the same record in place). No-op without a
    /// cloud client, a current objective `created_at`, OR a tokio runtime (so
    /// sync unit tests never panic); a push error is logged and dropped.
    fn cloud_push_frame(&self, frame: &BroadcastFrame) {
        let Some(client) = &self.cloud else {
            return;
        };
        let Some(cycle_created) = self.objective.as_ref().and_then(|o| o.created_at.clone()) else {
            return;
        };
        let cycle = crate::cloud::build::cycle_key(&cycle_created);
        let envelope = match frame {
            BroadcastFrame::ActionRecorded { action, .. } => {
                crate::cloud::build::from_action(&self.project_id, &cycle, action)
            }
            BroadcastFrame::ObjectiveSet { objective } => {
                crate::cloud::build::from_objective(&self.project_id, &cycle, objective)
            }
            BroadcastFrame::ObjectiveShipped { .. } => match &self.objective {
                Some(objective) => {
                    crate::cloud::build::from_objective(&self.project_id, &cycle, objective)
                }
                None => return,
            },
            BroadcastFrame::TaskAdded { task_id, .. }
            | BroadcastFrame::TaskStarted { task_id }
            | BroadcastFrame::TaskCompleted { task_id }
            | BroadcastFrame::TaskBlocked { task_id, .. } => {
                match self.tasks.iter().find(|t| t.id == *task_id) {
                    Some(task) => crate::cloud::build::from_task(
                        &self.project_id,
                        &cycle,
                        &cycle_created,
                        task,
                    ),
                    None => return,
                }
            }
            BroadcastFrame::CheckRecorded { task_id, check } => {
                // Identity = the check's append-only index within its task;
                // the just-recorded check is the last one.
                let Some(task) = self.tasks.iter().find(|t| t.id == *task_id) else {
                    return;
                };
                let seq = task.checks.len().saturating_sub(1);
                crate::cloud::build::from_check(&self.project_id, &cycle, task_id, seq, check)
            }
            BroadcastFrame::Cleared => return,
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let client = client.clone();
        handle.spawn(async move {
            if let Err(e) = client.push(&envelope).await {
                tracing::warn!(target: "think_and_ship::cloud", "ship cloud push failed: {e}");
            }
        });
    }

    /// Map a mutation frame to an Agent Trace record and append it to the repo
    /// trace; commit the session on `ObjectiveShipped`. No-op without a sink.
    /// Fire-and-forget: every failure is logged at WARN and dropped so the
    /// mutation path is never affected. The frame→record mapping lives here
    /// (engine-side) so `infra::repo_sync` stays domain-free.
    fn mirror_frame_to_repo(&self, frame: &BroadcastFrame) {
        let Some(mirror) = &self.mirror else {
            return;
        };

        let task_payload = |task_id: &str| {
            self.tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| serde_json::to_value(t).unwrap_or(serde_json::Value::Null))
                .unwrap_or(serde_json::Value::Null)
        };

        // (kind, payload, files[], is_session_close)
        let (kind, payload, files, closes) = match frame {
            BroadcastFrame::ObjectiveSet { objective } => (
                "objective",
                serde_json::to_value(objective).unwrap_or(serde_json::Value::Null),
                vec![],
                false,
            ),
            BroadcastFrame::TaskAdded { task_id, .. }
            | BroadcastFrame::TaskStarted { task_id }
            | BroadcastFrame::TaskCompleted { task_id }
            | BroadcastFrame::TaskBlocked { task_id, .. } => {
                ("task", task_payload(task_id), vec![], false)
            }
            BroadcastFrame::ActionRecorded { action, .. } => {
                let model_id = std::env::var("THINK_AND_SHIP_MODEL_ID")
                    .ok()
                    .filter(|s| !s.is_empty());
                let files = action
                    .files_touched
                    .iter()
                    .map(|p| crate::infra::file_attribution(p, model_id.as_deref()))
                    .collect();
                (
                    "action",
                    serde_json::to_value(action).unwrap_or(serde_json::Value::Null),
                    files,
                    false,
                )
            }
            BroadcastFrame::CheckRecorded { check, .. } => (
                "check",
                serde_json::to_value(check).unwrap_or(serde_json::Value::Null),
                vec![],
                false,
            ),
            BroadcastFrame::ObjectiveShipped { .. } => (
                "objective",
                serde_json::to_value(&self.objective).unwrap_or(serde_json::Value::Null),
                vec![],
                true,
            ),
            // `Cleared` is a reset, not a trace event — nothing to record.
            BroadcastFrame::Cleared => return,
        };

        // Hand off to the worker thread; building the record (which shells git),
        // the append, and any commit all happen off this engine's lock.
        mirror.submit(crate::infra::MirrorJob {
            family: "ship",
            kind,
            session_id: self.project_id.clone(),
            shared: self.repo_shared,
            payload,
            files,
            closes,
        });
    }

    pub fn with_persistence(mut self, persistence: Persistence) -> Self {
        if let Some((obj, tasks, next_id)) = persistence.load(&self.project_id) {
            self.objective = obj;
            self.tasks = tasks;
            self.next_action_id = next_id;
            eprintln!("ship: loaded {} task(s) from disk", self.tasks.len());
        }
        self.persistence = Some(persistence);
        self
    }

    fn persist(&self) {
        if let Some(p) = &self.persistence {
            p.save(
                &self.project_id,
                &self.objective,
                &self.tasks,
                self.next_action_id,
            );
        }
    }

    /// Define the objective for a development cycle. Returns the number of
    /// stale tasks cleared.
    ///
    /// If the *previous* objective was already finished (`Completed` /
    /// `Abandoned`), this is a brand-new cycle: its tasks are wiped so a fresh
    /// objective never inherits the prior cycle's `explore`/`implement`/`verify`
    /// tasks. That bleed corrupted a real run once — a new
    /// objective piled tasks onto a completed objective, and
    /// reused task ids then collided. While an objective is still in flight
    /// (`Defined`/`Active`) overwriting it preserves tasks, as documented.
    pub fn set_objective(
        &mut self,
        description: String,
        acceptance_criteria: Vec<String>,
        constraints: Vec<String>,
        scope: String,
    ) -> usize {
        let prior_finished = matches!(
            self.objective.as_ref().map(|o| &o.status),
            Some(ObjectiveStatus::Completed) | Some(ObjectiveStatus::Abandoned)
        );
        let mut cleared = 0;
        if prior_finished {
            cleared = self.tasks.len();
            self.tasks.clear();
            self.next_action_id = 1;
        }
        let now = Utc::now().to_rfc3339();
        self.objective = Some(Objective {
            description,
            acceptance_criteria,
            constraints,
            scope,
            status: ObjectiveStatus::Defined,
            project_id: self.project_id.clone(),
            created_at: Some(now),
            completed_at: None,
        });
        self.persist();
        if let Some(obj) = &self.objective {
            self.broadcast(BroadcastFrame::ObjectiveSet {
                objective: obj.clone(),
            });
        }
        cleared
    }

    pub fn add_task(
        &mut self,
        id: String,
        title: String,
        task_type: TaskType,
        estimate: Option<String>,
        think_branch: Option<String>,
    ) {
        if let Some(obj) = &mut self.objective
            && obj.status == ObjectiveStatus::Defined
        {
            obj.status = ObjectiveStatus::Active;
        }
        self.tasks.push(Task {
            id,
            title,
            task_type,
            status: TaskStatus::Planned,
            estimate,
            started_at: None,
            completed_at: None,
            artifacts: Vec::new(),
            checks: Vec::new(),
            actions: Vec::new(),
            blocked_reason: None,
            think_branch,
        });
        self.persist();
        let t = self.tasks.last().unwrap();
        self.broadcast(BroadcastFrame::TaskAdded {
            task_id: t.id.clone(),
            title: t.title.clone(),
        });
    }

    pub fn remove_task(&mut self, task_id: &str) -> Result<(), String> {
        let idx = self.task_index(task_id)?;
        let status = &self.tasks[idx].status;
        if *status == TaskStatus::Active || *status == TaskStatus::Completed {
            return Err(format!(
                "cannot remove task '{task_id}' with status {status:?}"
            ));
        }
        self.tasks.remove(idx);
        self.persist();
        Ok(())
    }

    pub fn reorder_task(&mut self, task_id: &str, after: Option<&str>) -> Result<(), String> {
        let idx = self.task_index(task_id)?;
        let task = self.tasks.remove(idx);
        let insert_at = match after {
            Some(after_id) => {
                let after_idx = self
                    .tasks
                    .iter()
                    .position(|t| t.id == after_id)
                    .ok_or_else(|| format!("task '{after_id}' not found"))?;
                after_idx + 1
            }
            None => 0,
        };
        self.tasks.insert(insert_at, task);
        self.persist();
        Ok(())
    }

    pub fn start_task(&mut self, task_id: &str) -> Result<&Task, String> {
        if let Some(active) = self.tasks.iter().find(|t| t.status == TaskStatus::Active) {
            return Err(format!(
                "task '{}' is already active — complete or block it first",
                active.id
            ));
        }
        let idx = self.task_index(task_id)?;
        let task = &mut self.tasks[idx];
        if task.status != TaskStatus::Planned && task.status != TaskStatus::Blocked {
            return Err(format!(
                "task '{task_id}' has status {:?}, cannot start",
                task.status
            ));
        }
        task.status = TaskStatus::Active;
        task.started_at = Some(Utc::now().to_rfc3339());
        task.blocked_reason = None;
        self.persist();
        self.broadcast(BroadcastFrame::TaskStarted {
            task_id: self.tasks[idx].id.clone(),
        });
        Ok(&self.tasks[idx])
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_action(
        &mut self,
        task_id: Option<&str>,
        action_type: ActionType,
        description: String,
        files_touched: Vec<String>,
        tools_used: Vec<String>,
        result: String,
        think_step: Option<u32>,
    ) -> Result<&Action, String> {
        let tid = self.resolve_task_id(task_id)?;
        let idx = self.task_index(&tid)?;
        let action_id = self.next_action_id;
        self.next_action_id += 1;
        let action = Action {
            id: action_id,
            task_id: tid,
            timestamp: Utc::now().to_rfc3339(),
            action_type,
            description,
            files_touched,
            tools_used,
            result,
            think_step,
        };
        self.tasks[idx].actions.push(action);
        self.persist();
        let recorded = self.tasks[idx].actions.last().unwrap();
        self.broadcast(BroadcastFrame::ActionRecorded {
            task_id: recorded.task_id.clone(),
            action: recorded.clone(),
        });
        Ok(self.tasks[idx].actions.last().unwrap())
    }

    pub fn complete_task(
        &mut self,
        task_id: &str,
        artifacts: Vec<Artifact>,
    ) -> Result<&Task, String> {
        let idx = self.task_index(task_id)?;
        let task = &mut self.tasks[idx];
        if task.status != TaskStatus::Active && task.status != TaskStatus::Blocked {
            return Err(format!(
                "task '{task_id}' has status {:?}, cannot complete",
                task.status
            ));
        }
        task.status = TaskStatus::Completed;
        task.completed_at = Some(Utc::now().to_rfc3339());
        task.blocked_reason = None;
        task.artifacts.extend(artifacts);
        self.persist();
        self.broadcast(BroadcastFrame::TaskCompleted {
            task_id: self.tasks[idx].id.clone(),
        });
        Ok(&self.tasks[idx])
    }

    pub fn block_task(&mut self, task_id: &str, reason: String) -> Result<&Task, String> {
        let idx = self.task_index(task_id)?;
        let task = &mut self.tasks[idx];
        if task.status != TaskStatus::Active {
            return Err(format!(
                "task '{task_id}' has status {:?}, only active tasks can be blocked",
                task.status
            ));
        }
        task.status = TaskStatus::Blocked;
        let reason_clone = reason.clone();
        task.blocked_reason = Some(reason);
        self.persist();
        self.broadcast(BroadcastFrame::TaskBlocked {
            task_id: self.tasks[idx].id.clone(),
            reason: reason_clone,
        });
        Ok(&self.tasks[idx])
    }

    /// Self-reported check (no command run). Thin wrapper over
    /// [`Self::record_check_full`] used by tests and manual gates.
    pub fn record_check(
        &mut self,
        task_id: Option<&str>,
        check_type: CheckType,
        name: String,
        passed: bool,
        details: String,
        required: bool,
    ) -> Result<&Check, String> {
        self.record_check_full(
            task_id, check_type, name, passed, details, required, false, None, None, None, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_check_full(
        &mut self,
        task_id: Option<&str>,
        check_type: CheckType,
        name: String,
        passed: bool,
        details: String,
        required: bool,
        verified: bool,
        command: Option<String>,
        exit_code: Option<i32>,
        report: Option<crate::ship::report::ReportRecord>,
        results: Option<crate::ship::report::TestResults>,
    ) -> Result<&Check, String> {
        let tid = self.resolve_task_id(task_id)?;
        let idx = self.task_index(&tid)?;
        let check = Check {
            check_type,
            name,
            passed,
            details,
            required,
            verified,
            command,
            exit_code,
            report,
            results,
            timestamp: Utc::now().to_rfc3339(),
        };
        self.tasks[idx].checks.push(check);
        self.persist();
        let recorded = self.tasks[idx].checks.last().unwrap();
        self.broadcast(BroadcastFrame::CheckRecorded {
            task_id: self.tasks[idx].id.clone(),
            check: recorded.clone(),
        });
        Ok(self.tasks[idx].checks.last().unwrap())
    }

    pub fn ship(&mut self, artifacts: Vec<Artifact>, summary: Option<String>) -> serde_json::Value {
        let mut warnings: Vec<String> = Vec::new();

        let total_tasks = self.tasks.len();
        let completed = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .count();
        let incomplete: Vec<&str> = self
            .tasks
            .iter()
            .filter(|t| t.status != TaskStatus::Completed && t.status != TaskStatus::Skipped)
            .map(|t| t.id.as_str())
            .collect();

        if !incomplete.is_empty() {
            warnings.push(format!(
                "{} task(s) not completed: {}",
                incomplete.len(),
                incomplete.join(", ")
            ));
        }

        let mut failed_required: Vec<String> = Vec::new();
        let mut unverified_required: Vec<String> = Vec::new();
        for task in &self.tasks {
            for check in &task.checks {
                if check.required && !check.passed {
                    failed_required.push(format!("{} (task: {})", check.name, task.id));
                } else if check.required && check.passed && !check.verified {
                    unverified_required.push(format!("{} (task: {})", check.name, task.id));
                }
            }
        }
        if !failed_required.is_empty() {
            warnings.push(format!(
                "required checks failed: {}",
                failed_required.join(", ")
            ));
        }
        if !unverified_required.is_empty() {
            // Self-reported green gates are exactly how a non-compiling phase
            // shipped with "tests pass". Surface them so a reviewer can tell a
            // run command apart from an agent's claim.
            warnings.push(format!(
                "required checks passed but were self-reported (not verified by running a command): {}",
                unverified_required.join(", ")
            ));
        }

        if let Some(obj) = &mut self.objective {
            obj.status = ObjectiveStatus::Completed;
            obj.completed_at = Some(Utc::now().to_rfc3339());
        }
        self.persist();
        self.broadcast(BroadcastFrame::ObjectiveShipped {
            warnings: warnings.clone(),
        });

        let all_artifacts: Vec<&Artifact> = self
            .tasks
            .iter()
            .flat_map(|t| &t.artifacts)
            .chain(artifacts.iter())
            .collect();

        serde_json::json!({
            "status": "shipped",
            "summary": summary,
            "tasks": { "total": total_tasks, "completed": completed },
            "artifacts_count": all_artifacts.len(),
            "ship_artifacts": artifacts,
            "warnings": warnings,
        })
    }

    pub fn status(&self) -> serde_json::Value {
        let active_task = self.tasks.iter().find(|t| t.status == TaskStatus::Active);

        let status_counts = serde_json::json!({
            "planned": self.tasks.iter().filter(|t| t.status == TaskStatus::Planned).count(),
            "active": self.tasks.iter().filter(|t| t.status == TaskStatus::Active).count(),
            "blocked": self.tasks.iter().filter(|t| t.status == TaskStatus::Blocked).count(),
            "completed": self.tasks.iter().filter(|t| t.status == TaskStatus::Completed).count(),
            "skipped": self.tasks.iter().filter(|t| t.status == TaskStatus::Skipped).count(),
            "total": self.tasks.len(),
        });

        let recent_actions: Vec<&Action> = self
            .tasks
            .iter()
            .flat_map(|t| &t.actions)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .take(5)
            .collect();

        let all_checks: Vec<serde_json::Value> = self
            .tasks
            .iter()
            .flat_map(|t| {
                t.checks.iter().map(move |c| {
                    serde_json::json!({
                        "task_id": t.id,
                        "name": c.name,
                        "type": c.check_type,
                        "passed": c.passed,
                        "required": c.required,
                        "verified": c.verified,
                    })
                })
            })
            .collect();

        let all_artifacts: Vec<serde_json::Value> = self
            .tasks
            .iter()
            .flat_map(|t| {
                t.artifacts.iter().map(move |a| {
                    serde_json::json!({
                        "task_id": t.id,
                        "type": a.artifact_type,
                        "ref": a.reference,
                        "description": a.description,
                    })
                })
            })
            .collect();

        let think_refs: Vec<serde_json::Value> = self
            .tasks
            .iter()
            .flat_map(|t| {
                let mut refs = Vec::new();
                if let Some(branch) = &t.think_branch {
                    refs.push(serde_json::json!({
                        "task_id": &t.id,
                        "ref_type": "branch",
                        "value": branch,
                    }));
                }
                for action in &t.actions {
                    if let Some(step) = action.think_step {
                        refs.push(serde_json::json!({
                            "task_id": &t.id,
                            "action_id": action.id,
                            "ref_type": "step",
                            "value": step,
                        }));
                    }
                }
                refs
            })
            .collect();

        serde_json::json!({
            "project_id": self.project_id,
            "objective": self.objective,
            "tasks": status_counts,
            "task_list": self.tasks.iter().map(|t| serde_json::json!({
                "id": t.id,
                "title": t.title,
                "type": t.task_type,
                "status": t.status,
                "estimate": t.estimate,
                "actions_count": t.actions.len(),
                "checks_count": t.checks.len(),
                "artifacts_count": t.artifacts.len(),
            })).collect::<Vec<_>>(),
            "active_task": active_task,
            "recent_actions": recent_actions,
            "checks": all_checks,
            "artifacts": all_artifacts,
            "think_refs": think_refs,
        })
    }

    pub fn export(&self, format: &str) -> String {
        match format {
            "json" => serde_json::to_string_pretty(&self.status()).unwrap_or_default(),
            _ => self.export_markdown(),
        }
    }

    pub fn reset(&mut self) {
        if let Some(p) = &self.persistence {
            p.clear(&self.project_id);
        }
        self.objective = None;
        self.tasks.clear();
        self.next_action_id = 1;
        self.broadcast(BroadcastFrame::Cleared);
    }

    pub fn plan_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "tasks": self.tasks.iter().map(|t| serde_json::json!({
                "id": t.id,
                "title": t.title,
                "type": t.task_type,
                "status": t.status,
                "estimate": t.estimate,
            })).collect::<Vec<_>>(),
            "total": self.tasks.len(),
        })
    }

    fn task_index(&self, task_id: &str) -> Result<usize, String> {
        self.tasks
            .iter()
            .position(|t| t.id == task_id)
            .ok_or_else(|| format!("task '{task_id}' not found"))
    }

    fn resolve_task_id(&self, explicit: Option<&str>) -> Result<String, String> {
        if let Some(id) = explicit {
            return Ok(id.to_string());
        }
        self.tasks
            .iter()
            .find(|t| t.status == TaskStatus::Active)
            .map(|t| t.id.clone())
            .ok_or_else(|| "no task_id provided and no active task".to_string())
    }

    fn export_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Execution Trace\n\n");

        if let Some(obj) = &self.objective {
            out.push_str(&format!("## Objective: {}\n", obj.description));
            out.push_str(&format!("Status: {:?}\n\n", obj.status));
            if !obj.acceptance_criteria.is_empty() {
                out.push_str("### Acceptance Criteria\n");
                for c in &obj.acceptance_criteria {
                    out.push_str(&format!("- {c}\n"));
                }
                out.push('\n');
            }
        }

        out.push_str("## Tasks\n\n");
        for task in &self.tasks {
            let status_icon = match task.status {
                TaskStatus::Completed => "[x]",
                TaskStatus::Active => "[>]",
                TaskStatus::Blocked => "[!]",
                TaskStatus::Skipped => "[-]",
                TaskStatus::Planned => "[ ]",
            };
            out.push_str(&format!(
                "- {status_icon} **{}** ({})\n",
                task.title, task.id
            ));

            for action in &task.actions {
                let step_ref = action
                    .think_step
                    .map(|s| format!(" (think #{s})"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  - {:?}: {}{}\n",
                    action.action_type, action.description, step_ref
                ));
            }
            for check in &task.checks {
                let icon = if check.passed { "pass" } else { "FAIL" };
                let req = if check.required { " (required)" } else { "" };
                out.push_str(&format!("  - [{icon}] {}{req}\n", check.name));
            }
            for artifact in &task.artifacts {
                out.push_str(&format!(
                    "  - artifact: {:?} {}\n",
                    artifact.artifact_type, artifact.reference
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod wire_id_tests {
    use super::*;

    /// The gate edge must carry the cycle-scoped form a check edge uses —
    /// a bare local id like `verify` recurs across cycles and resolves to
    /// nothing.
    #[test]
    fn active_task_wire_id_is_cycle_scoped() {
        let mut engine = ShipEngine::new("think-and-ship-test".into());
        engine.set_objective("chunk:x — test".into(), vec![], vec![], String::new());
        engine.add_task(
            "verify".into(),
            "Verify".into(),
            TaskType::Review,
            None,
            None,
        );
        engine.start_task("verify").expect("task starts");

        let wire = engine
            .active_task_wire_id()
            .expect("active task has a wire id");
        let created = engine
            .objective
            .as_ref()
            .and_then(|o| o.created_at.clone())
            .expect("objective is stamped");
        let cycle = crate::cloud::build::cycle_key(&created);
        assert_eq!(wire, format!("{cycle}.verify"));
        assert_ne!(wire, "verify", "the local id alone is not a resolvable ref");
    }

    /// With no active task there is no ref to carry — the edge is omitted
    /// rather than invented.
    #[test]
    fn no_active_task_means_no_wire_id() {
        let mut engine = ShipEngine::new("think-and-ship-test".into());
        engine.set_objective("chunk:x — test".into(), vec![], vec![], String::new());
        assert_eq!(engine.active_task_wire_id(), None);
    }
}
