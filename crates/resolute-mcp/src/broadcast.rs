use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

use crate::domain::action::Action;
use crate::domain::check::Check;
use crate::domain::objective::Objective;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BroadcastFrame {
    ObjectiveSet { objective: Objective },
    TaskAdded { task_id: String, title: String },
    TaskStarted { task_id: String },
    ActionRecorded { task_id: String, action: Action },
    CheckRecorded { task_id: String, check: Check },
    TaskCompleted { task_id: String },
    TaskBlocked { task_id: String, reason: String },
    ObjectiveShipped { warnings: Vec<String> },
    Cleared,
}

#[derive(Clone)]
pub struct Broadcaster {
    tx: mpsc::UnboundedSender<BroadcastFrame>,
}

impl Broadcaster {
    pub fn spawn(path: PathBuf) -> Option<Self> {
        if tokio::runtime::Handle::try_current().is_err() {
            return None;
        }

        if std::fs::symlink_metadata(&path)
            .is_ok_and(|m| std::os::unix::fs::FileTypeExt::is_socket(&m.file_type()))
        {
            let _ = std::fs::remove_file(&path);
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                warn!(target: "resolute::broadcast", "could not bind {}: {e}", path.display());
                return None;
            }
        };

        let (tx, rx) = mpsc::unbounded_channel::<BroadcastFrame>();
        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));

        tokio::spawn(accept_loop(listener, Arc::clone(&clients)));
        tokio::spawn(fanout_loop(rx, clients));

        info!(target: "resolute::broadcast", "listening at {}", path.display());
        Some(Self { tx })
    }

    pub fn emit(&self, frame: BroadcastFrame) {
        if self.tx.send(frame).is_err() {
            warn!(target: "resolute::broadcast", "channel closed; dropping frame");
        }
    }
}

async fn accept_loop(listener: UnixListener, clients: Arc<Mutex<Vec<UnixStream>>>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                debug!(target: "resolute::broadcast", "client connected");
                clients.lock().await.push(stream);
            }
            Err(e) => {
                warn!(target: "resolute::broadcast", "accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn fanout_loop(
    mut rx: mpsc::UnboundedReceiver<BroadcastFrame>,
    clients: Arc<Mutex<Vec<UnixStream>>>,
) {
    while let Some(frame) = rx.recv().await {
        let mut line = match serde_json::to_string(&frame) {
            Ok(s) => s,
            Err(e) => {
                warn!(target: "resolute::broadcast", "encode failed: {e}");
                continue;
            }
        };
        line.push('\n');
        let bytes = line.as_bytes();

        let mut guard = clients.lock().await;
        let taken = std::mem::take(&mut *guard);
        let mut survivors = Vec::with_capacity(taken.len());
        for mut stream in taken {
            match stream.write_all(bytes).await {
                Ok(()) => survivors.push(stream),
                Err(_) => {
                    debug!(target: "resolute::broadcast", "client disconnected");
                }
            }
        }
        *guard = survivors;
    }
}
