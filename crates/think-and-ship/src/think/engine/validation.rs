//! Step validation and the XML-injection recovery method.
//!
//! Three groups live here:
//!
//! * **Soft prefix/purpose validators** (`validate_thought_prefix`,
//!   `validate_rationale`, `validate_purpose`) gate the strict-mode
//!   prose checks. Each returns `true` when the corresponding config
//!   toggle is off.
//! * **Hard validators** (`validate_required_fields`,
//!   `validate_confidence`, `validate_dependencies`) return
//!   `Result<(), String>` and refuse a step that's structurally broken.
//! * [`ReasoningServer::recover_xml_injection`] mutates a step in place
//!   to patch up sibling parameters the harness's XML parser dropped.
//!   The extractors it leans on live in [`super::recovery`]; the
//!   diagnostic prose in `validate_required_fields` checks the same
//!   markers to name the failure mode when recovery couldn't help.

use crate::think::constants::{CONFIDENCE_MAX, CONFIDENCE_MIN, VALID_PREFIXES, is_valid_purpose};
use crate::think::domain::{DeliberateStep, DepEdge, NextAction};

use super::core::ReasoningServer;
use super::recovery::{
    RECOVERABLE_FIELD_NAMES, extract_bare_field_tags, extract_injected_parameters,
    truncate_at_markup,
};

impl ReasoningServer {
    pub fn validate_thought_prefix(&self, thought: &str) -> bool {
        if !self.config.validation.require_thought_prefix {
            return true;
        }
        VALID_PREFIXES.iter().any(|p| thought.starts_with(p))
    }

    pub fn validate_rationale(&self, rationale: &str) -> bool {
        if !self.config.validation.require_rationale_prefix {
            return true;
        }
        rationale.starts_with("To ")
    }

    pub fn validate_purpose(&self, purpose: &str) -> bool {
        if self.config.validation.allow_custom_purpose {
            return true;
        }
        is_valid_purpose(purpose)
    }

    pub(crate) fn validate_dependencies(&self, step: &DeliberateStep) -> Result<(), String> {
        let Some(deps) = &step.dependencies else {
            return Ok(());
        };
        if deps.is_empty() {
            return Ok(());
        }
        let dep_steps: Vec<u32> = deps.iter().map(DepEdge::step).collect();

        if dep_steps.contains(&step.step_number) {
            let msg = format!(
                "Circular dependency: step {} cannot depend on itself",
                step.step_number
            );
            eprintln!("⚠️ {msg}");
            return Err(msg);
        }
        let future: Vec<u32> = dep_steps
            .iter()
            .copied()
            .filter(|d| *d >= step.step_number)
            .collect();
        if !future.is_empty() {
            let joined = future
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let msg = format!(
                "Invalid dependencies: step {} cannot depend on future steps {joined}",
                step.step_number
            );
            eprintln!("⚠️ {msg}");
            return Err(msg);
        }
        let missing: Vec<u32> = dep_steps
            .iter()
            .copied()
            .filter(|d| !self.step_numbers.contains(d))
            .collect();
        if !missing.is_empty() {
            let joined = missing
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let available = self.sorted_step_numbers();
            eprintln!(
                "⚠️ Missing dependencies: steps {joined} not found. Available: {}",
                if available.is_empty() {
                    "none".into()
                } else {
                    available
                }
            );
        }
        Ok(())
    }

    /// Best-effort recovery from the harness's XML-injection failure
    /// mode. When the agent writes literal tool-call markup like a
    /// closing parameter tag inside another parameter's value, the
    /// harness terminates that parameter early and parses the
    /// "remainder" as new sibling parameters — which then get dropped
    /// because they aren't part of this tool call any more. By the
    /// time the step arrives at us, the intended sibling values are
    /// embedded as text in the surviving field's contents.
    ///
    /// We scan each surviving text field with both extractors in
    /// [`super::recovery`] and use the captured values to fill in any
    /// of the step's empty fields. Then we truncate the source field at
    /// the first markup boundary so the recovered values aren't
    /// duplicated as garbage in the persisted thought.
    ///
    /// Returns the list of field names that were successfully
    /// recovered — empty when no recovery happened. The caller surfaces
    /// this as a warning in the step's response.
    pub(crate) fn recover_xml_injection(step: &mut DeliberateStep) -> Vec<&'static str> {
        let sources: Vec<String> = vec![
            step.thought.clone(),
            step.context.clone(),
            step.outcome.clone(),
            step.rationale.clone(),
            step.purpose.clone(),
        ];

        let mut recovered: Vec<&'static str> = Vec::new();
        let mut any_extracted = false;

        // Two extractors: the primary catches the
        // `<parameter name="X">VALUE</parameter>` form (Claude Code's
        // literal wire syntax leaked into a value); the secondary
        // catches the bare `<X>VALUE</X>` form that's empirically more
        // common (agents serialize their tool call inside `thought`
        // using bracketed field names as section headers).
        let mut all_pairs: Vec<(String, String)> = Vec::new();
        for source in &sources {
            let primary = extract_injected_parameters(source);
            let secondary = extract_bare_field_tags(source);
            if !primary.is_empty() || !secondary.is_empty() {
                any_extracted = true;
            }
            all_pairs.extend(primary);
            all_pairs.extend(secondary);
        }

        for (name, value) in all_pairs {
            let v = value.trim();
            if v.is_empty() {
                continue;
            }
            match name.as_str() {
                "outcome" if step.outcome.trim().is_empty() => {
                    step.outcome = v.to_string();
                    recovered.push("outcome");
                }
                "rationale" if step.rationale.trim().is_empty() => {
                    step.rationale = v.to_string();
                    recovered.push("rationale");
                }
                "next_action" if matches!(&step.next_action, NextAction::Text(s) if s.trim().is_empty()) =>
                {
                    step.next_action = NextAction::Text(v.to_string());
                    recovered.push("next_action");
                }
                "context" if step.context.trim().is_empty() => {
                    step.context = v.to_string();
                    recovered.push("context");
                }
                "purpose" if step.purpose.trim().is_empty() => {
                    step.purpose = v.to_string();
                    recovered.push("purpose");
                }
                "thought" if step.thought.trim().is_empty() => {
                    step.thought = v.to_string();
                    recovered.push("thought");
                }
                "confidence" if step.confidence.is_none() => {
                    if let Ok(c) = v.parse::<f64>() {
                        step.confidence = Some(c);
                        recovered.push("confidence");
                    }
                }
                "uncertainty_notes" if step.uncertainty_notes.is_none() => {
                    step.uncertainty_notes = Some(v.to_string());
                    recovered.push("uncertainty_notes");
                }
                // The fields below are routinely included when the
                // agent serializes the full tool call inside `thought`.
                // JSON-ish values are best-effort parsed; if the agent's
                // encoding was malformed we silently skip — the
                // recovery is additive, never destructive.
                "dependencies" if step.dependencies.is_none() => {
                    if let Ok(deps) = serde_json::from_str::<Vec<DepEdge>>(v) {
                        step.dependencies = Some(deps);
                        recovered.push("dependencies");
                    }
                }
                "tools_used" if step.tools_used.is_none() => {
                    if let Ok(tools) = serde_json::from_str::<Vec<String>>(v) {
                        step.tools_used = Some(tools);
                        recovered.push("tools_used");
                    }
                }
                "pinned" if step.pinned.is_none() => {
                    if let Ok(b) = v.parse::<bool>() {
                        step.pinned = Some(b);
                        recovered.push("pinned");
                    }
                }
                "is_final_step" if step.is_final_step.is_none() => {
                    if let Ok(b) = v.parse::<bool>() {
                        step.is_final_step = Some(b);
                        recovered.push("is_final_step");
                    }
                }
                "branch_id" if step.branch_id.is_none() => {
                    step.branch_id = Some(v.to_string());
                    recovered.push("branch_id");
                }
                "branch_name" if step.branch_name.is_none() => {
                    step.branch_name = Some(v.to_string());
                    recovered.push("branch_name");
                }
                "branch_from" if step.branch_from.is_none() => {
                    if let Ok(n) = v.parse::<u32>() {
                        step.branch_from = Some(n);
                        recovered.push("branch_from");
                    }
                }
                "revises_step" if step.revises_step.is_none() => {
                    if let Ok(n) = v.parse::<u32>() {
                        step.revises_step = Some(n);
                        recovered.push("revises_step");
                    }
                }
                "revision_reason" if step.revision_reason.is_none() => {
                    step.revision_reason = Some(v.to_string());
                    recovered.push("revision_reason");
                }
                "session_id" if step.session_id.is_none() => {
                    step.session_id = Some(v.to_string());
                    recovered.push("session_id");
                }
                _ => {}
            }
        }

        // Even if we couldn't fill in a field (because either the name
        // didn't match or the target wasn't empty), we still want to
        // clean up the source fields so the literal markup doesn't
        // persist as noise. Only do this when we found markup at all.
        if any_extracted {
            step.thought = truncate_at_markup(&step.thought);
            step.context = truncate_at_markup(&step.context);
            step.outcome = truncate_at_markup(&step.outcome);
            step.rationale = truncate_at_markup(&step.rationale);
            step.purpose = truncate_at_markup(&step.purpose);
        }

        recovered
    }

    pub(crate) fn validate_required_fields(
        step: &DeliberateStep,
        recovered_fields: &[&'static str],
    ) -> Result<(), String> {
        let mut missing: Vec<String> = Vec::new();
        if step.step_number < 1 {
            missing.push("step_number (must be positive integer >= 1)".into());
        }
        if step.estimated_total < 1 {
            missing.push("estimated_total (must be positive integer >= 1)".into());
        }
        if step.purpose.trim().is_empty() {
            missing.push("purpose".into());
        }
        if step.context.trim().is_empty() {
            missing.push("context".into());
        }
        if step.thought.trim().is_empty() {
            missing.push("thought".into());
        }
        if step.outcome.trim().is_empty() {
            missing.push("outcome".into());
        }
        if step.rationale.trim().is_empty() {
            missing.push("rationale".into());
        }
        match &step.next_action {
            NextAction::Text(s) => {
                if s.trim().is_empty() {
                    missing.push("next_action".into());
                }
            }
            NextAction::Structured(a) => {
                if a.action.trim().is_empty() {
                    missing.push("next_action.action".into());
                }
            }
        }
        if missing.is_empty() {
            return Ok(());
        }

        // Detect the two empirically-observed failure modes when
        // recovery couldn't fill in the missing fields.
        //
        // Pattern A (most common in production traces — 24/24 of the
        // recoverable inputs from real rikttp sessions): the agent
        // serialized the full tool call inside `thought` using bare
        // `<outcome>...</outcome>` / `<rationale>...</rationale>` tags
        // as section headers. The harness passes the whole blob through
        // as one string and the actual sibling parameters are never
        // sent. We name the pattern explicitly so the agent's next turn
        // can stop using XML-tag formatting inside `thought`.
        //
        // Pattern B: literal Claude Code wire syntax appears inside a
        // value (`<parameter name="X">`, `<invoke name=...>`). The
        // harness closes the host parameter early and the intended
        // siblings get parsed as new top-level tags that get dropped.
        const PARAM_MARKERS: &[&str] = &[
            "<parameter name=",
            "</parameter>",
            "<invoke name=",
            "</invoke>",
        ];
        const TRUNC_MARKERS_DIAG: &[&str] = &["</thought>", "</invoke>", "</parameter>"];
        let scan: [(&str, &str); 5] = [
            ("purpose", step.purpose.as_str()),
            ("context", step.context.as_str()),
            ("thought", step.thought.as_str()),
            ("outcome", step.outcome.as_str()),
            ("rationale", step.rationale.as_str()),
        ];
        // Pattern-A signal: bare-tag markup still present in a surviving
        // field, OR recovery already extracted bare-tag content earlier
        // this call (the post-recovery truncate cleans the markup, so we
        // lean on `recovered_fields` as the after-the-fact signature).
        let recovered_indicates_pattern_a = !recovered_fields.is_empty();
        let pattern_a: Vec<&str> = scan
            .iter()
            .filter(|(_, v)| {
                TRUNC_MARKERS_DIAG.iter().any(|m| v.contains(m))
                    && RECOVERABLE_FIELD_NAMES
                        .iter()
                        .any(|n| v.contains(&format!("<{n}>")) || v.contains(&format!("</{n}>")))
            })
            .map(|(name, _)| *name)
            .collect();
        let pattern_b: Vec<&str> = scan
            .iter()
            .filter(|(_, v)| PARAM_MARKERS.iter().any(|m| v.contains(m)))
            .map(|(name, _)| *name)
            .collect();

        let head = format!("Missing or invalid required fields: {}", missing.join(", "));

        if !pattern_a.is_empty() || recovered_indicates_pattern_a {
            let where_str = if pattern_a.is_empty() {
                "your input".to_string()
            } else {
                format!(
                    "your {}",
                    pattern_a
                        .iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(" and ")
                )
            };
            return Err(format!(
                "{head}\n\n\
                 Likely cause: {where_str} contained structural tags like \
                 `<outcome>...</outcome>`, `<rationale>...</rationale>`, \
                 `<next_action>...</next_action>` used as if they were section headers. \
                 Those are NOT a way to structure your reasoning — they collide with \
                 Claude Code's tool-call wire format and the actual sibling parameters \
                 never reach the server.\n\n\
                 Fix: pass `outcome`, `rationale`, `next_action`, etc. as actual top-level \
                 JSON parameters of this tool call (the same way you pass `purpose` and \
                 `context`), NOT as XML sections inside `thought`. Each field is a separate \
                 parameter, not a section inside another field. If you need to discuss tag \
                 syntax in prose, fence it in backticks (`<outcome>`) so the literal \
                 characters don't appear in any value.",
            ));
        }

        if !pattern_b.is_empty() {
            return Err(format!(
                "{head}\n\n\
                 Likely cause: field(s) {fields} contain literal Claude Code wire syntax \
                 (`<parameter name=...>` or `<invoke name=...>`). When that markup appears \
                 inside a parameter's value the harness silently closes the parameter early \
                 and the intended siblings get parsed as new top-level tags that get \
                 dropped.\n\n\
                 Fix: describe markup in plain prose, or wrap it in backticks/different \
                 delimiters so the literal characters `<parameter` and `</parameter>` don't \
                 appear in any parameter value.",
                fields = pattern_b
                    .iter()
                    .map(|f| format!("'{f}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }

        Err(head)
    }

    pub(crate) fn validate_confidence(step: &DeliberateStep) -> Result<(), String> {
        let Some(c) = step.confidence else {
            return Ok(());
        };
        if !c.is_finite() {
            return Err(format!("Confidence must be a finite number, got {c}"));
        }
        if !(CONFIDENCE_MIN..=CONFIDENCE_MAX).contains(&c) {
            return Err(format!(
                "Confidence {c} out of bounds [{CONFIDENCE_MIN}, {CONFIDENCE_MAX}]"
            ));
        }
        Ok(())
    }
}
