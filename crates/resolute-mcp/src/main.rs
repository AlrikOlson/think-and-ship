use std::env;
use std::path::PathBuf;

use resolute_mcp::broadcast::Broadcaster;
use resolute_mcp::engine::ResoluteEngine;
use resolute_mcp::mcp::ResoluteService;
use resolute_mcp::persistence::{Persistence, PersistenceConfig};
use rmcp::{ServiceExt, transport::io::stdio};
use think_and_ship_core::resolve_project_id;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let project_id = resolve_project_id(Some("RESOLUTE_PROJECT_NAME"));
    let persist_cfg = PersistenceConfig::from_env();
    let persistence = Persistence::new(&persist_cfg);

    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    eprintln!("{pkg_name} {pkg_version} (project: {project_id})");
    if persist_cfg.enabled {
        eprintln!("  persistence: {}", persist_cfg.data_dir.display());
    }

    let mut engine = ResoluteEngine::new(project_id).with_persistence(persistence);

    if let Ok(socket_path) = env::var("RESOLUTE_BROADCAST_PATH") {
        let path = PathBuf::from(&socket_path);
        if let Some(broadcaster) = Broadcaster::spawn(path) {
            eprintln!("  broadcast: {socket_path}");
            engine = engine.with_broadcaster(broadcaster);
        }
    }

    let service = ResoluteService::new(engine);

    let (stdin, stdout) = stdio();
    let running = service.serve((stdin, stdout)).await?;
    eprintln!("resolute-mcp running on stdio");
    running.waiting().await?;
    Ok(())
}
