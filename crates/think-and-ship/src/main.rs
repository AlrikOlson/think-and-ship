use anyhow::Result;
use clap::{Parser, Subcommand};
use think_and_ship::cli;

#[derive(Parser, Debug)]
#[command(
    name = "think-and-ship",
    version,
    about = "Unified MCP server for structured reasoning + execution tracking"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run as an MCP server (stdio by default; --http for Streamable HTTP).
    Serve {
        /// Bind a Streamable HTTP listener at the given address (e.g. ":8080").
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
    },
    /// Set up project MCP config and optional CLAUDE.md.
    Init {
        /// Also write CLAUDE.md.
        #[arg(long)]
        with_claude_md: bool,
        /// MCP config and CLAUDE.md in one shot.
        #[arg(long)]
        full: bool,
    },
    /// Diagnose setup issues.
    Doctor,
    /// Show project info and config state.
    Status,
    /// Export traces.
    Export {
        /// Output format: markdown or json.
        #[arg(long, default_value = "markdown")]
        format: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve { http } => cli::serve(http),
        Command::Init {
            with_claude_md,
            full,
        } => cli::init(with_claude_md, full),
        Command::Doctor => cli::doctor(),
        Command::Status => cli::status(),
        Command::Export { format } => cli::export(&format),
    }
}
