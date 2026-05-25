//! `deliberate-mcp` binary entry point.

use anyhow::Result;
use deliberate_mcp::{config::load_config, server::ReasoningServer, tool::DeliberateService};
use rmcp::{ServiceExt, transport::io::stdio};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let config = load_config();

    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    eprintln!("🧠 {pkg_name} {pkg_version}");
    eprintln!("📋 Configuration:");
    eprintln!("   - Strict Mode: {}", config.validation.strict_mode);
    eprintln!(
        "   - Revisions: {}",
        if config.features.enable_revisions {
            "Enabled"
        } else {
            "Disabled"
        }
    );
    eprintln!(
        "   - Branching: {} (max depth: {})",
        if config.features.enable_branching {
            "Enabled"
        } else {
            "Disabled"
        },
        config.system.max_branch_depth
    );
    eprintln!(
        "   - Sessions: {} (timeout: {}min)",
        if config.features.enable_sessions {
            "Enabled"
        } else {
            "Disabled"
        },
        config.system.session_timeout
    );
    eprintln!(
        "   - Output Format: {}",
        config.display.output_format.as_str()
    );
    eprintln!("   - Max History: {} steps", config.system.max_history_size);

    let strict_mode = config.validation.strict_mode;
    let persist_msg = if config.persistence.enabled {
        format!(
            "   - Persistence: Enabled (data dir: {})",
            config.persistence.data_dir.display()
        )
    } else {
        "   - Persistence: Disabled (set DELIBERATE_PERSIST=true to enable)".to_string()
    };
    eprintln!("{persist_msg}");

    let engine = ReasoningServer::new(config);
    let service = DeliberateService::new(engine);

    let (stdin, stdout) = stdio();
    let running = service.serve((stdin, stdout)).await?;
    eprintln!("✅ deliberate MCP server running on stdio");
    if strict_mode {
        eprintln!("⚠️ Running in STRICT MODE - validation rules enforced");
    } else {
        eprintln!("🎯 Running in FLEXIBLE MODE - natural language allowed");
    }
    running.waiting().await?;
    Ok(())
}
