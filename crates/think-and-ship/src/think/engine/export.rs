//! Export the trace in human-readable formats — markdown, console, JSON,
//! and the ASCII branch tree. Wraps [`super::snapshots::history_with_branches`]
//! and dispatches to the appropriate [`Formatter`] method per format.

use crate::think::config::OutputFormat;
use crate::think::domain::DeliberateStep;

use super::core::ReasoningServer;

impl ReasoningServer {
    /// ASCII art of the branch hierarchy.
    pub fn branch_tree(&self) -> String {
        let history = self.history_with_branches();
        self.formatter.format_branch_tree(&history)
    }

    /// Render the trace in the requested output format.
    pub fn export_history(&self, format: OutputFormat) -> String {
        let snapshot = self.history_with_branches();
        match format {
            OutputFormat::Markdown => snapshot
                .steps
                .iter()
                .map(|s| self.formatter.format_step_markdown(s))
                .collect::<Vec<_>>()
                .join("\n\n"),
            OutputFormat::Console => snapshot
                .steps
                .iter()
                .map(|s| self.formatter.format_step_console(s))
                .collect::<Vec<_>>()
                .join("\n\n"),
            OutputFormat::Json => {
                serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".into())
            }
        }
    }

    /// Stderr-side single-step formatter used by `process_step` while
    /// recording. Format follows `config.display.output_format`.
    pub(crate) fn format_output(&self, step: &DeliberateStep) -> String {
        match self.config.display.output_format {
            OutputFormat::Json => self.formatter.format_step_json(step),
            OutputFormat::Markdown => self.formatter.format_step_markdown(step),
            OutputFormat::Console => self.formatter.format_step_console(step),
        }
    }
}
