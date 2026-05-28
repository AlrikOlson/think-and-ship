//! CLI subcommand handlers.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    ServiceExt,
    transport::{
        io::stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::env_compat::translate_legacy_env_vars;
use crate::infra::{
    Broadcaster as EngineBroadcaster, PersistenceConfig as InfraPersistenceConfig, RepoSink,
    SyncTarget, discover_repo_root, shared_from_env,
};
use crate::mcp::UnifiedService;
use crate::migrate::migrate_v0_1_data;
use crate::ship::ShipService;
use crate::ship::broadcast::Broadcaster as ShipBroadcaster;
use crate::ship::engine::ShipEngine;
use crate::ship::persistence::{
    Persistence as ShipPersistence, PersistenceConfig as ShipPersistenceConfig,
};
use crate::think::ThinkService;
use crate::think::broadcast::Broadcaster as ThinkBroadcaster;
use crate::think::config::load_config as load_think_config;
use crate::think::engine::core::ReasoningServer;

const UNIMPLEMENTED: &str = "think-and-ship: command not yet implemented.";

/// Default bind address when `--http` is passed without a value or with just a
/// port suffix (`:8080`).
const DEFAULT_HTTP_HOST: &str = "127.0.0.1";

pub fn serve(http: Option<String>) -> Result<()> {
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
        .block_on(async move {
            let (unified, project_id) = build_unified()?;
            let pkg_name = env!("CARGO_PKG_NAME");
            let pkg_version = env!("CARGO_PKG_VERSION");
            eprintln!("{pkg_name} {pkg_version} (project: {project_id})");

            match http {
                None => run_stdio(unified).await,
                Some(spec) => {
                    let addr = parse_http_addr(&spec)?;
                    run_http(addr, unified).await
                }
            }
        })
}

/// Build the unified MCP service from env-driven config. Returns the service
/// alongside the resolved project id so the caller can print a banner.
fn build_unified() -> Result<(UnifiedService, String)> {
    let mut think_config = load_think_config();

    // Spawn the broadcast socket ONCE so both families share a single
    // listener. Clear the path on the think config so ReasoningServer::new
    // doesn't try to bind it a second time — the shared handle is attached
    // below via with_broadcaster instead.
    let shared_broadcast = think_config
        .broadcast
        .path
        .clone()
        .and_then(EngineBroadcaster::spawn);
    if shared_broadcast.is_some()
        && let Some(path) = think_config.broadcast.path.as_ref()
    {
        eprintln!("broadcast: {} (shared by think + ship)", path.display());
    }
    think_config.broadcast.path = None;

    // Git-native trace sink (Phase 23b): when THINK_AND_SHIP_SYNC_TARGET=repo-git
    // and we're inside a git repo, mirror traces into `.think-and-ship/`. Both
    // families share one sink so they commit into the same repo tree.
    let repo_sink = resolve_repo_sink();

    let think_engine = {
        let mut server = ReasoningServer::new(think_config);
        if let Some(b) = shared_broadcast.clone() {
            server = server.with_broadcaster(ThinkBroadcaster::from_engine(b));
        }
        if let Some((sink, shared)) = repo_sink.clone() {
            server = server.with_repo_sink(sink, shared);
        }
        server
    };
    let think_service = ThinkService::new(think_engine);

    let project_id = crate::infra::resolve_project_id(None);
    let ship_persist_cfg = ShipPersistenceConfig::from_env();
    let ship_persistence = ShipPersistence::new(&ship_persist_cfg);
    let mut ship_engine = ShipEngine::new(project_id.clone()).with_persistence(ship_persistence);
    if let Some(b) = shared_broadcast {
        ship_engine = ship_engine.with_broadcaster(ShipBroadcaster::from_engine(b));
    }
    if let Some((sink, shared)) = repo_sink {
        ship_engine = ship_engine.with_repo_sink(sink, shared);
    }
    let ship_service = ShipService::new(ship_engine);

    Ok((UnifiedService::new(think_service, ship_service), project_id))
}

/// Resolve the optional git-native trace sink from the environment.
///
/// Returns `Some((sink, shared))` only when `THINK_AND_SHIP_SYNC_TARGET=repo-git`
/// AND the process is running inside a git repository. Otherwise `None` — the
/// engines fall back to plain XDG persistence (the `Local` default). `shared`
/// comes from `THINK_AND_SHIP_SHARED` (default `false` → gitignored `local/`).
fn resolve_repo_sink() -> Option<(RepoSink, bool)> {
    if SyncTarget::from_env() != SyncTarget::RepoGit {
        return None;
    }
    let cwd = std::env::current_dir().ok()?;
    let root = discover_repo_root(&cwd).or_else(|| {
        eprintln!(
            "think-and-ship: THINK_AND_SHIP_SYNC_TARGET=repo-git but not inside a git \
             repository — falling back to local persistence."
        );
        None
    })?;
    let shared = shared_from_env();
    let partition = if shared {
        "sessions (committed)"
    } else {
        "local (gitignored)"
    };
    eprintln!(
        "git-native traces: {}/.think-and-ship/ → {partition}",
        root.display()
    );
    Some((RepoSink::new(root), shared))
}

async fn run_stdio(unified: UnifiedService) -> Result<()> {
    let (stdin, stdout) = stdio();
    let running = unified.serve((stdin, stdout)).await?;
    eprintln!("think-and-ship running on stdio");
    running.waiting().await?;
    Ok(())
}

async fn run_http(addr: SocketAddr, unified: UnifiedService) -> Result<()> {
    let ct = CancellationToken::new();
    let mut config =
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token());
    // Both env-driven knobs *replace* rmcp's defaults. Unset → keep defaults
    // (loopback-only host validation, no Origin validation). README documents
    // that public deployments overriding ALLOWED_HOSTS lose the localhost
    // entry unless they include it explicitly.
    if let Some(hosts) = parse_csv_env("THINK_AND_SHIP_HTTP_ALLOWED_HOSTS") {
        eprintln!("http allowed hosts: {hosts:?}");
        config = config.with_allowed_hosts(hosts);
    }
    if let Some(origins) = parse_csv_env("THINK_AND_SHIP_HTTP_ALLOWED_ORIGINS") {
        eprintln!("http allowed origins: {origins:?}");
        config = config.with_allowed_origins(origins);
    }

    let http_service = StreamableHttpService::new(
        move || Ok::<_, std::io::Error>(unified.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", http_service);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP listener on {addr}"))?;
    let bound = listener
        .local_addr()
        .with_context(|| "reading bound HTTP local_addr")?;
    eprintln!("think-and-ship http on http://{bound}/mcp");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            ct.cancel();
        })
        .await?;
    Ok(())
}

/// Parse a comma-separated env var into a Vec of trimmed, non-empty entries.
/// Returns None when the var is unset, empty, or contains only whitespace and
/// commas (so the caller can leave the rmcp config default in place).
fn parse_csv_env(name: &str) -> Option<Vec<String>> {
    let raw = std::env::var(name).ok()?;
    let entries: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

/// Accept three input shapes:
/// - `:8080`         → `127.0.0.1:8080`
/// - `8080`          → `127.0.0.1:8080`
/// - `host:port`     → parsed as-is
fn parse_http_addr(spec: &str) -> Result<SocketAddr> {
    let spec = spec.trim();
    let normalized = if let Some(port) = spec.strip_prefix(':') {
        format!("{DEFAULT_HTTP_HOST}:{port}")
    } else if spec.parse::<u16>().is_ok() {
        format!("{DEFAULT_HTTP_HOST}:{spec}")
    } else {
        spec.to_string()
    };
    normalized.parse().with_context(|| {
        format!("invalid --http address {spec:?} (expected host:port, :port, or port)")
    })
}

fn init_tracing() {
    // Best-effort: if a global subscriber is already installed (e.g. by
    // tests), don't fail.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_addr_accepts_full_host_port() {
        let got = parse_http_addr("0.0.0.0:9000").unwrap();
        assert_eq!(got.to_string(), "0.0.0.0:9000");
    }

    #[test]
    fn parse_http_addr_accepts_colon_port_shorthand() {
        let got = parse_http_addr(":8080").unwrap();
        assert_eq!(got.to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn parse_http_addr_accepts_bare_port() {
        let got = parse_http_addr("8080").unwrap();
        assert_eq!(got.to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn parse_http_addr_rejects_garbage() {
        assert!(parse_http_addr("not-an-address").is_err());
    }

    // Process env is shared — fold the four parse_csv_env scenarios into
    // one sequential test so they don't race on the same key.
    #[test]
    fn parse_csv_env_covers_unset_empty_single_and_list() {
        const KEY: &str = "THINK_AND_SHIP_TEST_CSV_PARSE";

        // (1) Unset → None
        unsafe { std::env::remove_var(KEY) };
        assert_eq!(parse_csv_env(KEY), None);

        // (2) Empty / whitespace-only → None
        unsafe { std::env::set_var(KEY, "   ,  ,") };
        assert_eq!(parse_csv_env(KEY), None);

        // (3) Single value, with surrounding whitespace
        unsafe { std::env::set_var(KEY, "  https://app.example.com  ") };
        assert_eq!(
            parse_csv_env(KEY),
            Some(vec!["https://app.example.com".to_string()])
        );

        // (4) Comma-separated list with mixed whitespace + an empty slot
        unsafe { std::env::set_var(KEY, "a.example.com, b.example.com,,c.example.com ") };
        assert_eq!(
            parse_csv_env(KEY),
            Some(vec![
                "a.example.com".to_string(),
                "b.example.com".to_string(),
                "c.example.com".to_string(),
            ])
        );

        unsafe { std::env::remove_var(KEY) };
    }
}
