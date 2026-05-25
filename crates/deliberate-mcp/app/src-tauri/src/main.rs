//! `deliberate-app` — live viewer for the `deliberate-mcp` reasoning trace.
//!
//! Architecture:
//!
//!  - On startup we identify the on-disk sessions directory and the
//!    optional broadcast socket path (env-driven, same conventions as the
//!    MCP server itself), then load any persisted history into memory.
//!  - A background task subscribes to the broadcast socket if available
//!    and applies frames incrementally; another task watches the sessions
//!    directory for writes and re-loads changed files. Both feed a single
//!    orchestrator channel.
//!  - The orchestrator updates the shared [`AppState`] and emits Tauri
//!    events the frontend listens to. There are no polling loops.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod source;
mod state;

use std::sync::Arc;

use tauri::Emitter;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

use crate::source::Orchestrator;
use crate::state::AppState;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("deliberate_app=info,warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let state = Arc::new(Mutex::new(AppState::discover()));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::get_step_impact,
            commands::get_checkpoint,
            commands::search,
            commands::source_info,
            commands::reveal_data_dir,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let state = state.clone();
            tauri::async_runtime::spawn(async move {
                let orchestrator = Orchestrator::new(state.clone(), handle.clone());
                if let Err(e) = orchestrator.run().await {
                    tracing::error!("orchestrator failed: {e}");
                    let _ = handle.emit("orchestrator://fatal", e.to_string());
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running deliberate-app");
}
