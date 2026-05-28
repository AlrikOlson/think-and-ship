use chrono::Utc;

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
    /// Optional git-native trace sink (Phase 23b). When set, every mutation is
    /// mirrored into `.think-and-ship/` as an Agent Trace JSONL record and the
    /// session is committed on `ship_finalize`. `None` = the default Local
    /// behaviour. Writes are fire-and-forget — a sink error never fails a tool.
    repo_sink: Option<crate::infra::RepoSink>,
    /// Whether mirrored records are `shared` (committed `sessions/`) vs `local`
    /// (gitignored). Default `false`. Only meaningful with `repo_sink`.
    repo_shared: bool,
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
            repo_sink: None,
            repo_shared: false,
        }
    }

    pub fn with_broadcaster(mut self, broadcaster: Broadcaster) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Attach a git-native trace sink so mutations are mirrored into the repo's
    /// `.think-and-ship/` as Agent Trace JSONL. `shared` selects the committed
    /// `sessions/` partition (`true`) vs the gitignored `local/` partition
    /// (`false`). Wired by `cli::build_unified`.
    pub fn with_repo_sink(mut self, sink: crate::infra::RepoSink, shared: bool) -> Self {
        self.repo_sink = Some(sink);
        self.repo_shared = shared;
        self
    }

    fn broadcast(&self, frame: BroadcastFrame) {
        // Mirror into the git-native trace first (Phase 23b), then fan out to
        // the socket. Both are fire-and-forget.
        self.mirror_frame_to_repo(&frame);
        if let Some(b) = &self.broadcaster {
            b.emit(frame);
        }
    }

    /// Map a mutation frame to an Agent Trace record and append it to the repo
    /// trace; commit the session on `ObjectiveShipped`. No-op without a sink.
    /// Fire-and-forget: every failure is logged at WARN and dropped so the
    /// mutation path is never affected. The frame→record mapping lives here
    /// (engine-side) so `infra::repo_sync` stays domain-free.
    fn mirror_frame_to_repo(&self, frame: &BroadcastFrame) {
        let Some(sink) = &self.repo_sink else {
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

        let session_id = self.project_id.clone();
        let ctx = crate::infra::RecordCtx::resolve(sink.repo_root());
        let record = ctx.build_record("ship", kind, &session_id, self.repo_shared, payload, files);

        if let Err(e) = sink.append(&session_id, self.repo_shared, &record) {
            tracing::warn!(
                target: "think_and_ship::ship::repo_sync",
                "dropping git-native trace append: {e}",
            );
            return;
        }

        if closes && self.repo_shared {
            if let Err(e) = sink.commit_session(&session_id) {
                tracing::warn!(
                    target: "think_and_ship::ship::repo_sync",
                    "git-native trace commit failed: {e}",
                );
            }
        }
    }

    pub fn with_persistence(mut self, persistence: Persistence) -> Self {
        if let Some((obj, tasks, next_id)) = persistence.load(&self.project_id) {
            self.objective = obj;
            self.tasks = tasks;
            self.next_action_id = next_id;
            eprintln!(
                "resolute-mcp: loaded {} task(s) from disk",
                self.tasks.len()
            );
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

    pub fn set_objective(
        &mut self,
        description: String,
        acceptance_criteria: Vec<String>,
        constraints: Vec<String>,
        scope: String,
    ) {
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
    }

    pub fn add_task(
        &mut self,
        id: String,
        title: String,
        task_type: TaskType,
        estimate: Option<String>,
        deliberate_branch: Option<String>,
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
            deliberate_branch,
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
        deliberate_step: Option<u32>,
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
            deliberate_step,
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

    pub fn record_check(
        &mut self,
        task_id: Option<&str>,
        check_type: CheckType,
        name: String,
        passed: bool,
        details: String,
        required: bool,
    ) -> Result<&Check, String> {
        let tid = self.resolve_task_id(task_id)?;
        let idx = self.task_index(&tid)?;
        let check = Check {
            check_type,
            name,
            passed,
            details,
            required,
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
        for task in &self.tasks {
            for check in &task.checks {
                if check.required && !check.passed {
                    failed_required.push(format!("{} (task: {})", check.name, task.id));
                }
            }
        }
        if !failed_required.is_empty() {
            warnings.push(format!(
                "required checks failed: {}",
                failed_required.join(", ")
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

        let deliberate_refs: Vec<serde_json::Value> = self
            .tasks
            .iter()
            .flat_map(|t| {
                let mut refs = Vec::new();
                if let Some(branch) = &t.deliberate_branch {
                    refs.push(serde_json::json!({
                        "task_id": &t.id,
                        "ref_type": "branch",
                        "value": branch,
                    }));
                }
                for action in &t.actions {
                    if let Some(step) = action.deliberate_step {
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
            "deliberate_refs": deliberate_refs,
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
                    .deliberate_step
                    .map(|s| format!(" (deliberate #{s})"))
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
