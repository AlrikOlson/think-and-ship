//! CLI subcommand handlers.

use anyhow::{Result, bail};
use rmcp::{ServiceExt, transport::io::stdio};
use tracing_subscriber::EnvFilter;

use crate::engine::PersistenceConfig as InfraPersistenceConfig;
use crate::env_compat::translate_legacy_env_vars;
use crate::mcp::UnifiedService;
use crate::migrate::migrate_v0_1_data;
use crate::ship::ShipService;
use crate::ship::engine::ShipEngine;
use crate::ship::persistence::{Persistence as ShipPersistence, PersistenceConfig as ShipPersistenceConfig};
use crate::think::ThinkService;
use crate::think::config::load_config as load_think_config;
use crate::think::engine::core::ReasoningServer;

const UNIMPLEMENTED: &str = "think-and-ship: command not yet implemented.";

pub fn serve(http: Option<String>) -> Result<()> {
    if http.is_some() {
        bail!("Streamable HTTP transport is not implemented yet; use stdio (omit --http).");
    }

    init_tracing();
    let translated = translate_legacy_env_vars();
    if !translated.is_empty() {
        tracing::info!(
            "translated {} legacy env var(s) at startup: {:?}",
            translated.len(),
            translated
        );
    }

    let data_dir = InfraPersistenceConfig::from_env().data_dir;
    match migrate_v0_1_data(&data_dir) {
        Ok(report) if report.moved > 0 || report.skipped > 0 => {
            tracing::info!(
                "v0.1.x migration: moved={} skipped={} (root={})",
                report.moved,
                report.skipped,
                data_dir.display()
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("v0.1.x migration failed: {e}"),
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_server())
}

async fn run_server() -> Result<()> {
    let think_config = load_think_config();
    let think_engine = ReasoningServer::new(think_config);
    let think_service = ThinkService::new(think_engine);

    let project_id = crate::engine::resolve_project_id(None);
    let ship_persist_cfg = ShipPersistenceConfig::from_env();
    let ship_persistence = ShipPersistence::new(&ship_persist_cfg);
    let ship_engine = ShipEngine::new(project_id.clone()).with_persistence(ship_persistence);
    let ship_service = ShipService::new(ship_engine);

    let unified = UnifiedService::new(think_service, ship_service);

    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    eprintln!("{pkg_name} {pkg_version} (project: {project_id})");

    let (stdin, stdout) = stdio();
    let running = unified.serve((stdin, stdout)).await?;
    eprintln!("think-and-ship running on stdio");
    running.waiting().await?;
    Ok(())
}

fn init_tracing() {
    // Best-effort: if a global subscriber is already installed (e.g. by
    // tests), don't fail.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

pub fn init(_with_claude_md: bool, _full: bool) -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}

pub fn doctor() -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}

pub fn status() -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}

pub fn export(_format: &str) -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}
