//! Tauri commands invoked from the frontend. All read-only — mutating
//! the trace from the GUI is explicitly out of scope for v1.

use std::collections::HashMap;
use std::sync::Arc;

use deliberate_mcp::server::ReasoningServer;
use deliberate_mcp::types::Branch;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::state::{AppState, SourceInfo};

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub session_id: String,
    pub history: deliberate_mcp::types::DeliberateHistory,
    pub branches: Vec<Branch>,
}

#[derive(Debug, Serialize)]
pub struct SnapshotResponse {
    pub source: SourceInfo,
    pub active_session: String,
    pub sessions: Vec<Snapshot>,
}

#[tauri::command]
pub async fn get_snapshot(state: tauri::State<'_, Arc<Mutex<AppState>>>) -> Result<SnapshotResponse, String> {
    let s = state.lock().await;
    let sessions: Vec<Snapshot> = s
        .sessions
        .iter()
        .map(|(key, snap)| Snapshot {
            session_id: key.clone(),
            history: snap.history.clone(),
            branches: snap.branches.values().cloned().collect(),
        })
        .collect();
    Ok(SnapshotResponse {
        source: s.source.clone(),
        active_session: s.active_session.clone(),
        sessions,
    })
}

#[tauri::command]
pub async fn source_info(state: tauri::State<'_, Arc<Mutex<AppState>>>) -> Result<SourceInfo, String> {
    Ok(state.lock().await.source.clone())
}

#[tauri::command]
pub async fn get_step_impact(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
    step_number: u32,
) -> Result<Value, String> {
    let s = state.lock().await;
    let snap = s
        .session(&session_id)
        .ok_or_else(|| format!("unknown session: {session_id:?}"))?;
    let history = snap.history.clone();
    let branches: HashMap<String, Branch> = snap.branches.clone();
    drop(s);

    let server = ReasoningServer::for_analysis(history, branches);
    server.impact_of(step_number)
}

#[tauri::command]
pub async fn get_checkpoint(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
) -> Result<Value, String> {
    let s = state.lock().await;
    let snap = s
        .session(&session_id)
        .ok_or_else(|| format!("unknown session: {session_id:?}"))?;
    let history = snap.history.clone();
    let branches: HashMap<String, Branch> = snap.branches.clone();
    drop(s);

    let server = ReasoningServer::for_analysis(history, branches);
    Ok(server.checkpoint_snapshot())
}

#[tauri::command]
pub async fn search(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<Value>, String> {
    let s = state.lock().await;
    let snap = s
        .session(&session_id)
        .ok_or_else(|| format!("unknown session: {session_id:?}"))?;
    let history = snap.history.clone();
    let branches: HashMap<String, Branch> = snap.branches.clone();
    drop(s);

    let server = ReasoningServer::for_analysis(history, branches);
    let lim = limit.unwrap_or(10) as usize;
    Ok(server.search_steps(&query, lim))
}

#[tauri::command]
pub async fn reveal_data_dir(state: tauri::State<'_, Arc<Mutex<AppState>>>) -> Result<Option<String>, String> {
    let s = state.lock().await;
    Ok(s.source.data_dir.as_ref().map(|p| p.display().to_string()))
}
