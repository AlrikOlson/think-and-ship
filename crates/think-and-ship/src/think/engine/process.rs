//! `process_step` and its tightly-coupled helpers.
//!
//! `process_step` is the biggest single method on `ReasoningServer` —
//! it's the load-bearing entry point for `think_record_step`.
//! Recovery, validation, session lifecycle, branching, revisions, and
//! response enrichment all flow through it. The supporting helpers
//! that live alongside it here:
//!
//! * `Self::compute_step_warnings` — soft advisories surfaced in the
//!   step's response.
//! * `Self::make_error` — JSON-formatted `ProcessErr` builder.
//! * `Self::trim_history` — bounded-history sliding window.
//!
//! These four methods are kept together because they share private
//! mutable state and a lot of internal coupling. The rest of the
//! engine's methods live in concern-focused sibling modules
//! ([`super::validation`], [`super::branching`], [`super::sessions`],
//! [`super::revisions`], [`super::numbering`], [`super::lookup`],
//! [`super::impact`], [`super::snapshots`], [`super::mutations`],
//! [`super::export`], [`super::recovery`]).

use std::collections::HashSet;
use std::time::Instant;

use chrono::Utc;
use serde_json::{Map, Value, json};

use crate::think::broadcast::BroadcastFrame;
use crate::think::config::namespace_session_id;
use crate::think::constants::{
    COMPLETION_PHRASES, LOW_CONFIDENCE_THRESHOLD, VALID_PREFIXES, VALID_PURPOSES,
};
use crate::think::domain::ThinkStep;
use crate::think::util::text::truncate_excerpt;

use super::core::{ProcessErr, ProcessOk, ProcessResult, ReasoningServer};

impl ReasoningServer {
    fn trim_history(&mut self) {
        let max = self.config.system.max_history_size;
        if self.history.steps.len() <= max {
            return;
        }
        let drop_count = self.history.steps.len() - max;
        let removed_step_numbers: HashSet<u32> = self.history.steps[..drop_count]
            .iter()
            .map(|s| s.step_number)
            .collect();
        self.history.steps.drain(..drop_count);

        for n in &removed_step_numbers {
            self.step_index.remove(n);
            self.step_numbers.remove(n);
            self.step_to_branch.remove(n);
        }
        // Rebuild step_index because indexes shifted after drain.
        self.step_index.clear();
        for (idx, s) in self.history.steps.iter().enumerate() {
            self.step_index.insert(s.step_number, idx);
        }

        let removed_branches: Vec<(String, String, u32)> = self
            .branches
            .iter()
            .filter(|(_, b)| removed_step_numbers.contains(&b.from_step))
            .map(|(id, b)| (id.clone(), b.name.clone(), b.from_step))
            .collect();
        for (id, name, from_step) in removed_branches {
            if let Some(branch) = self.branches.remove(&id) {
                for s in &branch.steps {
                    self.step_to_branch.remove(&s.step_number);
                }
                eprintln!("🗑️ Branch \"{name}\" removed (from_step {from_step} was trimmed)");
            }
        }

        self.invalidate_branch_depth_cache();
        eprintln!(
            "📋 History trimmed to {} steps (removed {} old steps)",
            max,
            removed_step_numbers.len()
        );
    }

    pub fn process_step(&mut self, mut step: ThinkStep) -> ProcessResult {
        let step_start = Instant::now();

        // Try to recover from XML-injection corruption BEFORE validating.
        // If the agent's `thought` contains embedded `<parameter name=X>VALUE</parameter>`
        // segments, we extract them and fill in the corresponding empty
        // fields. The caller (the model) sees a successful step plus a
        // warning that we recovered — far better than a hard reject.
        let recovered_fields = Self::recover_xml_injection(&mut step);

        // Forgiving step numbering. A *new* step (not a revision/branch) that
        // either omits step_number (serde default 0) or reuses a number that's
        // already taken project-wide is the #1 failure mode for a fresh agent
        // chat that doesn't know the persistent project already has N steps.
        // Rather than hard-fail (which stalls the run), auto-assign the next
        // project-wide number and surface a warning. Revisions and branches
        // legitimately carry an explicit number and are left untouched here.
        // Doing this BEFORE validate_required_fields is deliberate: an omitted
        // (0) step_number would otherwise be rejected as "missing".
        let mut auto_numbered: Option<u32> = None;
        if step.revises_step.is_none()
            && step.branch_id.is_none()
            && step.branch_from.is_none()
            && (step.step_number == 0 || self.step_number_taken_in_project(step.step_number))
        {
            let requested = step.step_number;
            step.step_number = self.max_step_number_in_project().saturating_add(1);
            auto_numbered = Some(requested);
        }

        if let Err(e) = Self::validate_required_fields(&step, &recovered_fields) {
            return Err(self.make_error(&e));
        }
        if let Err(e) = Self::validate_confidence(&step) {
            return Err(self.make_error(&e));
        }

        if self.config.validation.strict_mode {
            if !self.validate_thought_prefix(&step.thought) {
                return Err(self.make_error(&format!(
                    "Thought must start with one of: {} (strict mode)",
                    VALID_PREFIXES.join(", ")
                )));
            }
            if !self.validate_rationale(&step.rationale) {
                return Err(self.make_error("Rationale must start with \"To \" (strict mode)"));
            }
            if !self.validate_purpose(&step.purpose) {
                return Err(self.make_error(&format!(
                    "Invalid purpose \"{}\". Valid: {} (strict mode)",
                    step.purpose,
                    VALID_PURPOSES.join(", ")
                )));
            }
        } else if !self.validate_purpose(&step.purpose) {
            eprintln!("⚠️ Using custom purpose: {}", step.purpose);
        }

        step.timestamp = Some(Utc::now().to_rfc3339());
        // Immutable wire identity, minted exactly once — the renumber may
        // rewrite step_number later, but never this (think-step-stable-id).
        step.record_id = Some(uuid::Uuid::new_v4().to_string());
        // Stamp project root so the step carries its provenance to
        // disk. Cached at server-construction time — one syscall per
        // server, not per step.
        if step.cwd.is_none() {
            step.cwd = self.cwd.clone();
        }

        // Resolve the session id for this step. Any caller-supplied id
        // gets auto-prefixed with the project namespace so agents can't
        // accidentally write outside their project's bucket — see
        // config::namespace_session_id. When sessions aren't enabled
        // we leave session_id alone (legacy single-history mode).
        if self.config.features.enable_sessions && !self.project_id.is_empty() {
            let raw = step
                .session_id
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| self.config.features.default_session_id.clone())
                .unwrap_or_else(|| self.project_id.clone());
            let resolved = namespace_session_id(&self.project_id, &raw);
            step.session_id = Some(resolved);
        }

        if let (Some(session_id), true) = (
            step.session_id.clone(),
            self.config.features.enable_sessions,
        ) {
            self.cleanup_expired_sessions(false);
            self.switch_to_session(&session_id);
        }

        if let Err(e) = self.validate_dependencies(&step) {
            return Err(self.make_error(&e));
        }

        // Backstop for project-wide step_number uniqueness. The top-of-function
        // pass already bumped omitted/colliding new-step numbers, but switching
        // to the target session above can surface steps that weren't visible
        // then (a lazily-loaded sibling session). If a new-step number is still
        // taken, auto-bump again rather than hard-fail — same forgiving policy.
        // Revisions and branches legitimately carry their own number, untouched.
        if step.revises_step.is_none()
            && step.branch_id.is_none()
            && step.branch_from.is_none()
            && self.step_number_taken_in_project(step.step_number)
        {
            let requested = step.step_number;
            step.step_number = self.max_step_number_in_project().saturating_add(1);
            // Preserve the originally-requested number if the top pass already
            // recorded one; otherwise record this collision.
            auto_numbered.get_or_insert(requested);
        }

        let completed = step.is_final_step.unwrap_or(false) || {
            let lower = step.thought.to_ascii_lowercase();
            COMPLETION_PHRASES.iter().any(|p| lower.contains(p))
        };
        if completed {
            self.history.completed = true;
        }

        if let Err(e) = self.handle_revision(&step) {
            return Err(self.make_error(&e));
        }

        if let Err(e) = self.handle_branching(&mut step) {
            return Err(self.make_error(&e));
        }

        let tools_used = Self::extract_tools_used(&step);
        for t in &tools_used {
            self.tools_used.insert(t.clone());
        }
        if let Some(meta) = self.history.metadata.as_mut() {
            let mut sorted: Vec<String> = self.tools_used.iter().cloned().collect();
            sorted.sort();
            meta.tools_used = Some(sorted);
            // Stamp the resolved project id so the viewer can group by
            // it without parsing the session-id filename. Cheap — only
            // overwrites when the value would actually change.
            if !self.project_id.is_empty()
                && meta.project_id.as_deref() != Some(self.project_id.as_str())
            {
                meta.project_id = Some(self.project_id.clone());
            }
        }

        step.duration_ms =
            Some(u64::try_from(step_start.elapsed().as_millis()).unwrap_or(u64::MAX));
        if let Some(meta) = self.history.metadata.as_mut() {
            meta.total_duration_ms =
                Some(u64::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u64::MAX));
        }
        self.history.updated_at = Some(Utc::now().to_rfc3339());

        let step_number = step.step_number;
        let next_action = step.next_action.clone();
        let confidence = step.confidence;
        let revises_step = step.revises_step;
        let branch_id = step.branch_id.clone();
        let branch_name = step.branch_name.clone();
        let branch_from = step.branch_from;
        let estimated_total = step.estimated_total;
        let low_confidence = confidence
            .map(|c| c < LOW_CONFIDENCE_THRESHOLD)
            .unwrap_or(false);
        let uncertainty_notes = step.uncertainty_notes.clone();

        let formatted = self.format_output(&step);
        eprintln!("{formatted}");
        if low_confidence {
            let pct = (confidence.unwrap_or(0.0) * 100.0).round() as i32;
            eprintln!(
                "⚠️ Low confidence ({pct}%): {}",
                uncertainty_notes
                    .as_deref()
                    .unwrap_or("Consider verification")
            );
        }

        // Push and index.
        let idx = self.history.steps.len();
        self.history.steps.push(step);
        self.step_index.insert(step_number, idx);
        self.step_numbers.insert(step_number);

        self.trim_history();

        // Mirror updated history back into the active session entry.
        if let Some(session_id) = self.active_session.clone() {
            if let Some(entry) = self.sessions.get_mut(&session_id) {
                entry.history = self.history.clone();
                entry.last_accessed = Self::now_ms();
            }
        }

        // Persist after every successful step. The active session (or default
        // history) is written atomically; failures are logged but don't fail
        // the step.
        self.persist_active();

        // Broadcast the append — and, when this step was itself a
        // revision, the back-pointer update on its target — so any live
        // observer sees the trace grow in real time. Fire-and-forget; no
        // observer = no work.
        if let Some(b) = &self.broadcaster {
            let session_id = self.active_session.clone();
            if let Some(stored) = self.history.steps.last() {
                b.emit(BroadcastFrame::StepAppended {
                    session_id: session_id.clone(),
                    step: Box::new(stored.clone()),
                });
            }
            if let Some(target) = revises_step {
                b.emit(BroadcastFrame::StepRevised {
                    session_id,
                    revised_step: target,
                    by_step: step_number,
                });
            }
        }

        // Mirror the step into the git-native trace when a repo
        // sink is attached. Fire-and-forget, exactly like the broadcaster:
        // any error is logged and dropped so the tool path never fails.
        self.mirror_step_to_repo();

        // Cloud sync: push the appended step to the cloud backend,
        // independent of the broadcaster — sync works with no viewer socket.
        self.cloud_push_last_step();

        // Response is intentionally tight — the caller already has the step
        // it sent in its own context, so we don't echo `next_action`,
        // top-level `confidence`, or `completed: false` (those would be
        // pure noise). We DO surface anything the server computed or
        // derived: step counts, branch id (auto-assigned), warnings,
        // recent_steps with state the caller couldn't easily reconstruct.
        let mut response: Map<String, Value> = Map::new();
        response.insert("step_number".into(), json!(step_number));
        response.insert("estimated_total".into(), json!(estimated_total));
        response.insert("total_steps".into(), json!(self.history.steps.len()));
        if self.history.completed {
            response.insert("completed".into(), json!(true));
        }
        let _ = next_action; // intentionally not echoed
        let _ = confidence; // top-level echo dropped; appears in recent_steps when relevant
        if let Some(r) = revises_step {
            response.insert("revised_step".into(), json!(r));
        }
        if let Some(id) = branch_id {
            let mut branch_obj = Map::new();
            branch_obj.insert("id".into(), json!(id));
            if let Some(name) = branch_name {
                branch_obj.insert("name".into(), json!(name));
            }
            if let Some(from) = branch_from {
                branch_obj.insert("from".into(), json!(from));
            }
            response.insert("branch".into(), Value::Object(branch_obj));
        }

        // ─── Response enrichments (see also `recent_steps_rollup`, `branches_summary`). ───
        // These give the model the context it needs to self-orient without
        // making a second call. Each field is added only when non-trivial so
        // we don't bloat token usage on simple traces.

        // Echo a short excerpt of what was just recorded — confirms the step
        // landed and gives the model a stable anchor it can refer to later.
        let last_idx = self.history.steps.len().saturating_sub(1);
        if let Some(stored) = self.history.steps.get(last_idx) {
            response.insert(
                "thought_excerpt".into(),
                json!(truncate_excerpt(&stored.thought, 120)),
            );
            response.insert(
                "outcome_excerpt".into(),
                json!(truncate_excerpt(&stored.outcome, 120)),
            );

            // Acknowledge optional inputs the model declared — if the model
            // sees `dependencies` or `tools_recorded` echoed, it knows the
            // server accepted them.
            if let Some(deps) = &stored.dependencies {
                if !deps.is_empty() {
                    response.insert("dependencies".into(), json!(deps));
                }
            }
        }
        if !tools_used.is_empty() {
            response.insert("tools_recorded".into(), json!(tools_used));
        }

        // Warnings: never an error, but flag situations the model should know
        // about (e.g. depending on prior reasoning that itself had low confidence).
        let mut warnings = self.compute_step_warnings(step_number, confidence);
        if let Some(requested) = auto_numbered {
            let how = if requested == 0 {
                "step_number was omitted".to_string()
            } else {
                format!("requested step_number {requested} was already taken")
            };
            warnings.insert(
                0,
                format!(
                    "Auto-assigned step_number {step_number} ({how}). Step numbers are \
                     unique project-wide; use this number for back-references."
                ),
            );
        }
        if !recovered_fields.is_empty() {
            warnings.insert(
                0,
                format!(
                    "Auto-recovered field(s) {recovered:?} from XML-injected text in your \
                     parameter values. Cause: literal `</parameter>` or `</thought>` inside \
                     a value closes that parameter early in the harness's tool-call parser. \
                     The siblings you intended to send were embedded as text in `thought` \
                     and we extracted them. Next time, describe markup in prose or fence it \
                     in backticks so the literal characters don't appear in any value.",
                    recovered = recovered_fields,
                ),
            );
        }
        if !warnings.is_empty() {
            response.insert("warnings".into(), json!(warnings));
        }

        // A rolling view of the last few prior steps — purpose, ~80-char
        // thought excerpt, confidence. Lets the model self-orient mid-trace
        // without having to call `think_export_trace`. Pinned steps are
        // promoted into this window so load-bearing conclusions don't fall
        // out as the trace lengthens.
        let limit = self.config.system.recent_steps_limit;
        let recent = self.recent_steps_rollup(limit, Some(step_number));
        if !recent.is_empty() {
            response.insert("recent_steps".into(), Value::Array(recent));
        }

        // SUMMARY-PLUS-HANDLE. When the budget dropped pinned steps, say so and
        // name the tool that fetches them. The loss then becomes DELIBERATE:
        // an agent that needs an older pinned conclusion is told it exists and
        // how to get it, instead of never learning it was there. Emitted only
        // when something was actually withheld, so short traces pay nothing.
        let withheld = self.pinned_withheld_count(limit, Some(step_number));
        if withheld > 0 {
            response.insert(
                "recent_steps_omitted".into(),
                json!({
                    "pinned_withheld": withheld,
                    "fetch_with": "think_get_step",
                    "note": "Older pinned steps were withheld to keep this response inside its size \
                             budget. They are still in the trace — fetch any of them by step_number \
                             with think_get_step, or list them with think_engine_status(verbose: true).",
                }),
            );
        }

        // Branch overview is always relevant when any branches exist, not
        // just on the step that created one.
        let branches_summary = self.branches_summary();
        if !branches_summary.is_empty() {
            response.insert("branches_summary".into(), Value::Array(branches_summary));
        }

        Ok(ProcessOk {
            text: serde_json::to_string_pretty(&Value::Object(response))
                .unwrap_or_else(|_| "{}".into()),
        })
    }

    /// Mirror the most-recently-recorded step into the git-native trace
    /// as an Agent Trace JSONL record. No-op when no `repo_sink`
    /// is attached. Fire-and-forget: every failure is logged at WARN and
    /// dropped so the `process_step` path is never affected. The session is
    /// committed when the step closes the trace (`is_final_step`) and records
    /// are `shared`.
    fn mirror_step_to_repo(&self) {
        let Some(mirror) = &self.mirror else {
            return;
        };
        let Some(stored) = self.history.steps.last() else {
            return;
        };
        // One JSONL file per session; fall back to the project id when no
        // named session is active so default-history steps still land somewhere.
        let session_id = self
            .active_session
            .clone()
            .unwrap_or_else(|| self.project_id.clone());

        let payload = serde_json::to_value(stored).unwrap_or(Value::Null);
        // Commit on session close, but only for the shared (committed)
        // partition — the local partition is gitignored. Hand off to the worker
        // thread; building the record (which shells git), the append, and any
        // commit all happen off this engine's lock.
        let closes = self.repo_shared && stored.is_final_step == Some(true);
        mirror.submit(crate::infra::MirrorJob {
            family: "think",
            kind: "step",
            session_id,
            shared: self.repo_shared,
            payload,
            files: vec![], // Reasoning steps attribute no code.
            closes,
        });
    }

    /// Soft, non-fatal advisories about the just-recorded step.
    /// Warning text is kept compact — the response is already keyed by
    /// `step_number`, so the redundant "Step N ..." prefix is dropped.
    fn compute_step_warnings(&self, step_number: u32, confidence: Option<f64>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let last_idx = self.history.steps.len().saturating_sub(1);
        let Some(stored) = self.history.steps.get(last_idx) else {
            return out;
        };

        if stored.session_id.is_some() && !self.config.features.enable_sessions {
            out.push(
                "session_id ignored — sessions are disabled (set THINK_AND_SHIP_ENABLE_SESSIONS=true). Step landed in the default history."
                    .into(),
            );
        }

        if let Some(deps) = &stored.dependencies {
            for edge in deps {
                let dep_n = edge.step();
                let relation = edge.relation();
                // Skip prior-confidence and revised-by checks for explicitly
                // "refutes" edges — refuting evidence is healthy and not a
                // shaky-dependency signal.
                let is_refutes = relation == Some("refutes");
                if let Some(&i) = self.step_index.get(&dep_n) {
                    if let Some(prior) = self.history.steps.get(i) {
                        if !is_refutes {
                            if let Some(prior_c) = prior.confidence {
                                if prior_c < LOW_CONFIDENCE_THRESHOLD {
                                    out.push(format!(
                                        "dep on step {dep_n} has low confidence ({}%) — validate before building",
                                        (prior_c * 100.0).round() as i32
                                    ));
                                }
                            }
                            if let Some(revised_by) = prior.revised_by {
                                out.push(format!(
                                    "dep on step {dep_n} was revised by step {revised_by} — re-check it still holds"
                                ));
                            }
                        }
                        // Refuted-prior advisory: this step depends on N
                        // (supportively / unlabeled) but some other step
                        // refutes N. Fire only for non-refutes edges.
                        if !is_refutes {
                            let refuted_by =
                                self.refuters_of(dep_n, /* exclude */ stored.step_number);
                            if !refuted_by.is_empty() {
                                let joined = refuted_by
                                    .iter()
                                    .map(|n| n.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                out.push(format!(
                                    "dep on step {dep_n} is refuted by step(s) {joined} — re-validate before relying on it"
                                ));
                            }
                        }
                    }
                }
            }
        }

        if let Some(c) = confidence {
            if c < LOW_CONFIDENCE_THRESHOLD && stored.uncertainty_notes.is_none() {
                out.push(format!(
                    "confidence is low ({}%) but no uncertainty_notes — record what's uncertain",
                    (c * 100.0).round() as i32
                ));
            }
        }

        if step_number > stored.estimated_total {
            out.push(format!(
                "step_number {step_number} exceeds estimated_total {} — call think_revise_estimate",
                stored.estimated_total
            ));
        }

        out
    }

    fn make_error(&self, msg: &str) -> ProcessErr {
        let hint = if self.config.validation.strict_mode {
            "Strict mode is enabled. Set THINK_AND_SHIP_STRICT_MODE=false for flexible validation."
        } else {
            "Check that all required fields are provided."
        };
        let body = json!({
            "error": msg,
            "status": "failed",
            "hint": hint,
        });
        ProcessErr {
            text: serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into()),
        }
    }
}

#[cfg(test)]
mod auto_numbering_tests {
    use super::ReasoningServer;
    use crate::think::config::ThinkConfig;
    use crate::think::domain::ThinkStep;
    use serde_json::{Value, json};

    fn quiet_config() -> ThinkConfig {
        let mut c = ThinkConfig::default();
        c.display.color_output = false;
        c.persistence.enabled = false;
        c
    }

    /// Build a valid new step, optionally omitting step_number (None → field
    /// absent, which deserializes to the serde default 0).
    fn step(step_number: Option<u32>) -> ThinkStep {
        let mut body = json!({
            "estimated_total": 10,
            "purpose": "analysis",
            "context": "ctx",
            "thought": "some reasoning",
            "outcome": "did a thing",
            "next_action": "next",
            "rationale": "because",
        });
        if let Some(n) = step_number {
            body["step_number"] = json!(n);
        }
        serde_json::from_value(body).expect("valid step json")
    }

    fn ok_json(server: &mut ReasoningServer, s: ThinkStep) -> Value {
        let ok = server.process_step(s).expect("step should be accepted");
        serde_json::from_str(&ok.text).expect("ok payload is json")
    }

    fn has_auto_warning(v: &Value) -> bool {
        v["warnings"]
            .as_array()
            .map(|w| {
                w.iter().any(|x| {
                    x.as_str()
                        .is_some_and(|s| s.contains("Auto-assigned step_number"))
                })
            })
            .unwrap_or(false)
    }

    #[test]
    fn omitted_step_number_is_auto_assigned() {
        let mut server = ReasoningServer::new(quiet_config());
        let first = ok_json(&mut server, step(None));
        assert_eq!(first["step_number"], 1, "first omitted step → 1");
        assert!(has_auto_warning(&first));

        let second = ok_json(&mut server, step(None));
        assert_eq!(second["step_number"], 2, "second omitted step → 2");
    }

    #[test]
    fn colliding_step_number_is_auto_bumped_not_rejected() {
        let mut server = ReasoningServer::new(quiet_config());
        let first = ok_json(&mut server, step(Some(1)));
        assert_eq!(first["step_number"], 1);

        // A fresh-chat-style retry of step 1 must NOT hard-fail — it bumps.
        let bumped = ok_json(&mut server, step(Some(1)));
        assert_eq!(
            bumped["step_number"], 2,
            "collision auto-bumps to next free"
        );
        assert!(has_auto_warning(&bumped));
    }

    #[test]
    fn explicit_unique_step_number_is_respected() {
        let mut server = ReasoningServer::new(quiet_config());
        let out = ok_json(&mut server, step(Some(1)));
        assert_eq!(out["step_number"], 1);
        // No auto-numbering warning when the caller's number is fine.
        assert!(!has_auto_warning(&out));
    }
}
