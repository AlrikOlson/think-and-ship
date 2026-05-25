//! Source orchestrator. Owns the two data inputs (broadcast socket and
//! fs-watcher) and merges them into a single event stream that updates
//! [`crate::state::AppState`] and emits Tauri events to the frontend.

pub mod file;
pub mod socket;

use std::sync::Arc;

use anyhow::Result;
use deliberate_mcp::broadcast::BroadcastFrame;
use deliberate_mcp::types::{Branch, BranchStatus, DeliberateHistory};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use crate::state::{AppState, SourceMode};

#[derive(Debug)]
pub enum SourceEvent {
    /// Real-time frame arrived from the broadcast socket.
    Frame(BroadcastFrame),
    /// Disk snapshot for a session — either the initial load or a
    /// fs-watcher-triggered refresh.
    Snapshot {
        session_id: Option<String>,
        history: DeliberateHistory,
    },
    /// The socket source connected.
    SocketConnected,
    /// The socket source disconnected (peer closed or never bound).
    SocketDisconnected,
}

/// Wire-format event the frontend listens to. The frontend keeps its own
/// shadow of session state and applies these events as a reducer.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrontendEvent {
    Snapshot {
        session_id: String,
        history: DeliberateHistory,
        branches: Vec<Branch>,
    },
    StepAppended {
        session_id: String,
        step: Box<deliberate_mcp::types::DeliberateStep>,
    },
    StepRevised {
        session_id: String,
        revised_step: u32,
        by_step: u32,
    },
    PinChanged {
        session_id: String,
        step_number: u32,
        pinned: bool,
    },
    EstimateRevised {
        session_id: String,
        old: u32,
        new: u32,
    },
    BranchStatusChanged {
        session_id: String,
        branch_id: String,
        status: BranchStatus,
        merged_into: Option<u32>,
    },
    Cleared {
        session_id: String,
    },
    SourceChanged {
        mode: SourceMode,
    },
}

pub struct Orchestrator {
    state: Arc<Mutex<AppState>>,
    handle: AppHandle,
}

impl Orchestrator {
    pub fn new(state: Arc<Mutex<AppState>>, handle: AppHandle) -> Self {
        Self { state, handle }
    }

    pub async fn run(self) -> Result<()> {
        // Emit the initial snapshot for every session loaded at startup.
        self.emit_initial_snapshots().await?;

        let (tx, mut rx) = mpsc::unbounded_channel::<SourceEvent>();

        // Spawn the socket source if a path is configured.
        let socket_path = self.state.lock().await.source.socket_path.clone();
        if let Some(path) = socket_path {
            let tx_s = tx.clone();
            tokio::spawn(async move {
                socket::run(path, tx_s).await;
            });
        }

        // Spawn the file watcher if a data dir is configured.
        let data_dir = self.state.lock().await.source.data_dir.clone();
        if let Some(dir) = data_dir {
            let tx_f = tx.clone();
            tokio::spawn(async move {
                file::run(dir, tx_f).await;
            });
        }

        drop(tx); // tasks own their own clones

        while let Some(event) = rx.recv().await {
            self.apply(event).await;
        }
        Ok(())
    }

    async fn emit_initial_snapshots(&self) -> Result<()> {
        let state = self.state.lock().await;
        for (key, snap) in state.sessions.iter() {
            // Skip the empty-default session if it has no steps — the
            // frontend's empty state will render an instruction screen.
            if key.is_empty() && snap.history.steps.is_empty() {
                continue;
            }
            let evt = FrontendEvent::Snapshot {
                session_id: key.clone(),
                history: snap.history.clone(),
                branches: snap.branches.values().cloned().collect(),
            };
            let _ = self.handle.emit("trace://event", &evt);
        }
        let _ = self.handle.emit(
            "trace://event",
            &FrontendEvent::SourceChanged {
                mode: state.source.mode,
            },
        );
        Ok(())
    }

    async fn apply(&self, event: SourceEvent) {
        match event {
            SourceEvent::Frame(frame) => self.apply_frame(frame).await,
            SourceEvent::Snapshot {
                session_id,
                history,
            } => self.apply_snapshot(session_id, history).await,
            SourceEvent::SocketConnected => self.note_source_change(true).await,
            SourceEvent::SocketDisconnected => self.note_source_change(false).await,
        }
    }

    async fn apply_frame(&self, frame: BroadcastFrame) {
        let mut state = self.state.lock().await;
        match frame {
            BroadcastFrame::StepAppended { session_id, step } => {
                let key = session_id.clone().unwrap_or_default();
                let snap = state.session_mut(&session_id);
                let cloned = step.clone();
                snap.apply_step_appended(*cloned);
                let _ = self.handle.emit(
                    "trace://event",
                    &FrontendEvent::StepAppended {
                        session_id: key,
                        step,
                    },
                );
            }
            BroadcastFrame::StepRevised {
                session_id,
                revised_step,
                by_step,
            } => {
                let key = session_id.clone().unwrap_or_default();
                let snap = state.session_mut(&session_id);
                snap.apply_step_revised(revised_step, by_step);
                let _ = self.handle.emit(
                    "trace://event",
                    &FrontendEvent::StepRevised {
                        session_id: key,
                        revised_step,
                        by_step,
                    },
                );
            }
            BroadcastFrame::BranchStatusChanged {
                session_id,
                branch_id,
                status,
                merged_into,
            } => {
                let key = session_id.clone().unwrap_or_default();
                let snap = state.session_mut(&session_id);
                snap.apply_branch_status(&branch_id, status, merged_into);
                let _ = self.handle.emit(
                    "trace://event",
                    &FrontendEvent::BranchStatusChanged {
                        session_id: key,
                        branch_id,
                        status,
                        merged_into,
                    },
                );
            }
            BroadcastFrame::PinChanged {
                session_id,
                step_number,
                pinned,
            } => {
                let key = session_id.clone().unwrap_or_default();
                let snap = state.session_mut(&session_id);
                snap.apply_pin_changed(step_number, pinned);
                let _ = self.handle.emit(
                    "trace://event",
                    &FrontendEvent::PinChanged {
                        session_id: key,
                        step_number,
                        pinned,
                    },
                );
            }
            BroadcastFrame::EstimateRevised {
                session_id,
                old,
                new,
                ..
            } => {
                let key = session_id.clone().unwrap_or_default();
                let snap = state.session_mut(&session_id);
                snap.apply_estimate_revised(old, new);
                let _ = self.handle.emit(
                    "trace://event",
                    &FrontendEvent::EstimateRevised {
                        session_id: key,
                        old,
                        new,
                    },
                );
            }
            BroadcastFrame::Cleared => {
                let key = state.active_session.clone();
                if let Some(snap) = state.sessions.get_mut(&key) {
                    snap.history.steps.clear();
                    snap.branches.clear();
                }
                let _ = self
                    .handle
                    .emit("trace://event", &FrontendEvent::Cleared { session_id: key });
            }
        }
    }

    async fn apply_snapshot(&self, session_id: Option<String>, history: DeliberateHistory) {
        let key = session_id.clone().unwrap_or_default();
        let branches_payload: Vec<Branch>;
        {
            let mut state = self.state.lock().await;
            let snap = state.session_mut(&session_id);
            snap.replace_from_history(history.clone());
            branches_payload = snap.branches.values().cloned().collect();
        }
        let _ = self.handle.emit(
            "trace://event",
            &FrontendEvent::Snapshot {
                session_id: key,
                history,
                branches: branches_payload,
            },
        );
    }

    async fn note_source_change(&self, socket_alive: bool) {
        let mut state = self.state.lock().await;
        let has_file = state.source.data_dir.is_some();
        let new_mode = match (socket_alive, has_file) {
            (true, true) => SourceMode::SocketAndFile,
            (true, false) => SourceMode::Socket,
            (false, true) => SourceMode::File,
            (false, false) => SourceMode::None,
        };
        if state.source.mode == new_mode {
            return;
        }
        state.source.mode = new_mode;
        let _ = self.handle.emit(
            "trace://event",
            &FrontendEvent::SourceChanged { mode: new_mode },
        );
    }
}
