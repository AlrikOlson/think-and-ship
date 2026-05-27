//! Console / Markdown / JSON formatters for reasoning steps.

use owo_colors::{OwoColorize, Style};

use crate::think::domain::{Branch, DeliberateHistory, DeliberateStep, NextAction};

#[derive(Debug, Clone, Copy)]
pub struct Formatter {
    color_enabled: bool,
}

impl Formatter {
    pub fn new(color_enabled: bool) -> Self {
        Self { color_enabled }
    }

    /// Construct a formatter that never emits ANSI escapes. Use this for MCP
    /// tool responses — the consuming LLM can't render colors and the bytes
    /// just bloat token count.
    pub fn plain() -> Self {
        Self {
            color_enabled: false,
        }
    }

    fn paint(&self, text: &str, style: Style) -> String {
        if self.color_enabled {
            text.style(style).to_string()
        } else {
            text.to_string()
        }
    }

    fn purpose_style(purpose: &str) -> Style {
        match purpose.to_ascii_lowercase().as_str() {
            "analysis" => Style::new().blue(),
            "action" => Style::new().green(),
            "reflection" => Style::new().yellow(),
            "decision" => Style::new().magenta(),
            "summary" => Style::new().cyan(),
            "validation" => Style::new().bright_green(),
            "exploration" => Style::new().bright_yellow(),
            "hypothesis" => Style::new().bright_blue(),
            "correction" => Style::new().bright_red(),
            "planning" => Style::new().bright_cyan(),
            _ => Style::new().white(),
        }
    }

    fn format_confidence(&self, confidence: Option<f64>) -> String {
        let Some(c) = confidence else {
            return String::new();
        };
        let percentage = (c * 100.0).round() as i32;
        let (symbol, style) = if c < 0.3 {
            ('○', Style::new().red())
        } else if c < 0.7 {
            ('◐', Style::new().yellow())
        } else {
            ('●', Style::new().green())
        };
        if self.color_enabled {
            format!(" {}", format!(" {symbol} {percentage}%").style(style))
        } else {
            format!(" [{percentage}%]")
        }
    }

    fn format_next_action(action: &NextAction) -> String {
        match action {
            NextAction::Text(s) => s.clone(),
            NextAction::Structured(a) => {
                let mut out = a.action.clone();
                if let Some(tool) = &a.tool {
                    out = format!("[{tool}] {out}");
                }
                if let Some(params) = &a.parameters {
                    if !params.is_empty() {
                        if let Ok(json) = serde_json::to_string(params) {
                            out.push_str(&format!(" ({json})"));
                        }
                    }
                }
                out
            }
        }
    }

    pub fn format_step_console(&self, step: &DeliberateStep) -> String {
        let purpose_style = Self::purpose_style(&step.purpose);
        let purpose_text = step.purpose.to_ascii_uppercase();
        let mut header = self.paint(
            &format!(
                "[Step {}/{}] {}",
                step.step_number, step.estimated_total, purpose_text
            ),
            purpose_style,
        );
        header.push_str(&self.format_confidence(step.confidence));

        if let Some(revises) = step.revises_step {
            header.push_str(&self.paint(&format!(" ↻ Revises #{revises}"), Style::new().yellow()));
        }
        if let Some(from) = step.branch_from {
            header
                .push_str(&self.paint(&format!(" ⟿ Branch from #{from}"), Style::new().magenta()));
            if let Some(name) = &step.branch_name {
                header.push_str(&self.paint(&format!(" ({name})"), Style::new().bright_black()));
            }
        }

        let mut lines = vec![header];

        let gray = Style::new().bright_black();
        let white = Style::new().white();
        let yellow = Style::new().yellow();

        if !step.context.is_empty() {
            lines.push(format!("{} {}", self.paint("Context:", gray), step.context));
        }

        lines.push(format!(
            "{} {}",
            self.paint("Thought:", white),
            step.thought
        ));

        if let Some(notes) = &step.uncertainty_notes {
            lines.push(format!("{} {}", self.paint("Uncertainty:", yellow), notes));
        }
        if let Some(reason) = &step.revision_reason {
            lines.push(format!(
                "{} {}",
                self.paint("Revision Reason:", yellow),
                reason
            ));
        }

        lines.push(format!("{} {}", self.paint("Outcome:", gray), step.outcome));

        let next_action = Self::format_next_action(&step.next_action);
        lines.push(format!(
            "{} {} - {}",
            self.paint("Next:", gray),
            next_action,
            step.rationale
        ));

        if let Some(tools) = &step.tools_used {
            if !tools.is_empty() {
                lines.push(format!(
                    "{} {}",
                    self.paint("Tools Used:", gray),
                    tools.join(", ")
                ));
            }
        }

        if let Some(deps) = &step.dependencies {
            if !deps.is_empty() {
                let joined = deps
                    .iter()
                    .map(|e| match e.relation() {
                        Some(rel) => format!("{}({})", e.step(), rel),
                        None => e.step().to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "{} Steps {}",
                    self.paint("Depends On:", gray),
                    joined
                ));
            }
        }

        if let Some(exec_ref) = &step.execution_ref {
            lines.push(format!(
                "{} {}",
                self.paint("Execution Ref:", gray),
                exec_ref
            ));
        }

        lines.push(self.paint(&"─".repeat(60), gray));
        lines.join("\n")
    }

    pub fn format_step_markdown(&self, step: &DeliberateStep) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "### Step {}/{}: {}",
            step.step_number,
            step.estimated_total,
            step.purpose.to_ascii_uppercase()
        ));

        let mut badges: Vec<String> = Vec::new();
        if let Some(c) = step.confidence {
            let pct = (c * 100.0).round() as i32;
            badges.push(format!(
                "![Confidence](https://img.shields.io/badge/confidence-{pct}%25-blue)"
            ));
        }
        if let Some(r) = step.revises_step {
            badges.push(format!(
                "![Revises](https://img.shields.io/badge/revises-step%20{r}-yellow)"
            ));
        }
        if let Some(b) = step.branch_from {
            badges.push(format!(
                "![Branch](https://img.shields.io/badge/branch-from%20{b}-purple)"
            ));
        }
        if !badges.is_empty() {
            lines.push(badges.join(" "));
        }

        lines.push(String::new());
        lines.push(format!("**Context:** {}", step.context));
        lines.push(String::new());
        lines.push(format!("**Thought:** {}", step.thought));

        if let Some(notes) = &step.uncertainty_notes {
            lines.push(String::new());
            lines.push(format!("> ⚠️ **Uncertainty:** {notes}"));
        }
        if let Some(reason) = &step.revision_reason {
            lines.push(String::new());
            lines.push(format!("> 🔄 **Revision Reason:** {reason}"));
        }

        lines.push(String::new());
        lines.push(format!("**Outcome:** {}", step.outcome));
        lines.push(String::new());

        let next_action = Self::format_next_action(&step.next_action);
        lines.push(format!("**Next Action:** {next_action}"));
        lines.push(format!("- *Rationale:* {}", step.rationale));

        if let Some(tools) = &step.tools_used {
            if !tools.is_empty() {
                lines.push(String::new());
                lines.push(format!("**Tools Used:** {}", tools.join(", ")));
            }
        }

        if let Some(exec_ref) = &step.execution_ref {
            lines.push(String::new());
            lines.push(format!("**Execution Ref:** `{exec_ref}`"));
        }

        lines.push(String::new());
        lines.push("---".to_string());
        lines.join("\n")
    }

    pub fn format_step_json(&self, step: &DeliberateStep) -> String {
        serde_json::to_string_pretty(step).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn format_history_summary(&self, history: &DeliberateHistory) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines.push(self.paint("=== Deliberation Summary ===", Style::new().bold()));
        lines.push(format!("Total Steps: {}", history.steps.len()));
        lines.push(format!(
            "Status: {}",
            if history.completed {
                "✓ Completed"
            } else {
                "⟳ In Progress"
            }
        ));

        if let Some(meta) = &history.metadata {
            if let Some(r) = meta.revisions_count {
                if r > 0 {
                    lines.push(format!("Revisions: {r}"));
                }
            }
            if let Some(b) = meta.branches_created {
                if b > 0 {
                    lines.push(format!("Branches Created: {b}"));
                }
            }
            if let Some(d) = meta.total_duration_ms {
                if d > 0 {
                    let secs = (d as f64) / 1000.0;
                    lines.push(format!("Duration: {secs:.2}s"));
                }
            }
            if let Some(tools) = &meta.tools_used {
                if !tools.is_empty() {
                    lines.push(format!("Tools Used: {}", tools.join(", ")));
                }
            }
        }

        let with_conf: Vec<f64> = history.steps.iter().filter_map(|s| s.confidence).collect();
        if !with_conf.is_empty() {
            let avg = with_conf.iter().sum::<f64>() / with_conf.len() as f64;
            lines.push(format!(
                "Average Confidence: {}%",
                (avg * 100.0).round() as i32
            ));
        }

        if let Some(branches) = &history.branches {
            if !branches.is_empty() {
                lines.push(String::new());
                lines.push("Branches:".to_string());
                for branch in branches {
                    let status = match branch.status {
                        crate::think::domain::BranchStatus::Active => '●',
                        crate::think::domain::BranchStatus::Merged => '✓',
                        crate::think::domain::BranchStatus::Abandoned => '✗',
                    };
                    lines.push(format!(
                        "  {} {} ({} steps)",
                        status,
                        branch.name,
                        branch.steps.len()
                    ));
                }
            }
        }

        lines.push(self.paint(&"=".repeat(30), Style::new().bright_black()));
        lines.join("\n")
    }

    pub fn format_branch_tree(&self, history: &DeliberateHistory) -> String {
        let mut lines = vec!["Branch Structure:".to_string()];
        let mut branches_by_step: std::collections::HashMap<u32, Vec<&Branch>> =
            std::collections::HashMap::new();
        if let Some(branches) = &history.branches {
            for b in branches {
                branches_by_step.entry(b.from_step).or_default().push(b);
            }
        }
        lines.push("Main:".to_string());
        for step in history.steps.iter().filter(|s| s.branch_id.is_none()) {
            lines.push(format!("  └─ Step {}: {}", step.step_number, step.purpose));
            if let Some(bs) = branches_by_step.get(&step.step_number) {
                for branch in bs {
                    lines.push(format!("     └─ Branch: {}", branch.name));
                    for bstep in &branch.steps {
                        lines.push(format!(
                            "        └─ Step {}: {}",
                            bstep.step_number, bstep.purpose
                        ));
                    }
                }
            }
        }
        lines.join("\n")
    }
}
