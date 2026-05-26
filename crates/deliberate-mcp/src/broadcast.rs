//! Best-effort, fire-and-forget broadcast of trace mutations over a Unix
//! domain socket. Used by the Tauri desktop viewer (and any other passive
//! observer) to see step appends, revisions, branch transitions, pins, and
//! clears in real time without polling.
//!
//! Wire format: newline-delimited JSON, one [`BroadcastFrame`] per line.
//! Clients connect to `$DELIBERATE_BROADCAST_PATH`; multiple clients may be
//! attached simultaneously. Slow or disconnected clients are dropped silently.
//!
//! Emission is never allowed to fail the calling MCP tool path:
//!
//! - Bind failure at startup returns `None` from [`Broadcaster::spawn`];
//!   [`ReasoningServer`] then runs without a broadcaster.
//! - Channel send failure is logged at `WARN` and discarded.
//! - Per-client write failure drops the client and continues.
//!
//! [`ReasoningServer`]: crate::server::ReasoningServer

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

use crate::types::{BranchStatus, DeliberateStep};

/// One mutation event. Encoded as a single JSON line with an externally
/// tagged `type` discriminant so a stream of mixed frames is unambiguous.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BroadcastFrame {
    /// A new step was appended to the trace (either main line or branch).
    StepAppended {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        step: Box<DeliberateStep>,
    },
    /// An older step's `revised_by` pointer was set. The revising step
    /// itself is delivered via a separate [`Self::StepAppended`] frame.
    StepRevised {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        revised_step: u32,
        by_step: u32,
    },
    /// A branch transitioned to a new status. `merged_into` is set only
    /// when `status == Merged` and the caller named a synthesis step.
    BranchStatusChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        branch_id: String,
        status: BranchStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        merged_into: Option<u32>,
    },
    /// The `pinned` flag on a step changed.
    PinChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        step_number: u32,
        pinned: bool,
    },
    /// The most-recent step's `estimated_total` was revised in place.
    EstimateRevised {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        old: u32,
        new: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The entire trace was wiped.
    Cleared,
}

/// Cheaply cloneable handle to the broadcaster's input channel. The accept
/// loop and fanout loop run on background tasks; the server side only ever
/// touches the channel sender.
#[derive(Clone)]
pub struct Broadcaster {
    tx: mpsc::UnboundedSender<BroadcastFrame>,
}

impl Broadcaster {
    /// Bind a Unix listener at `path` and spawn the accept + fanout tasks
    /// on the current tokio runtime. Returns `None` if binding fails or
    /// no runtime is active in the calling context. The server runs
    /// unaffected in either case.
    pub fn spawn(path: PathBuf) -> Option<Self> {
        if tokio::runtime::Handle::try_current().is_err() {
            warn!(
                target: "deliberate::broadcast",
                "no tokio runtime active; broadcaster disabled",
            );
            return None;
        }

        // A leftover socket file from a previous run blocks bind with
        // EADDRINUSE. The unlink is gated on the file actually being a
        // socket so we don't clobber an unrelated regular file at the
        // configured path.
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type()) {
                let _ = std::fs::remove_file(&path);
            }
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(
                    target: "deliberate::broadcast",
                    "could not create parent dir for broadcast socket {}: {e}",
                    path.display(),
                );
                return None;
            }
        }

        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                warn!(
                    target: "deliberate::broadcast",
                    "could not bind broadcast socket {}: {e}",
                    path.display(),
                );
                return None;
            }
        };

        let (tx, rx) = mpsc::unbounded_channel::<BroadcastFrame>();
        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));

        tokio::spawn(accept_loop(listener, Arc::clone(&clients)));
        tokio::spawn(fanout_loop(rx, clients));

        info!(
            target: "deliberate::broadcast",
            "broadcast socket listening at {}",
            path.display(),
        );
        Some(Self { tx })
    }

    /// Enqueue a frame for fanout. Returns immediately. Channel-send
    /// failure (receiver dropped — shouldn't happen in practice) is
    /// logged and discarded so the calling tool path is never affected.
    pub fn emit(&self, frame: BroadcastFrame) {
        if self.tx.send(frame).is_err() {
            warn!(
                target: "deliberate::broadcast",
                "broadcast channel closed; dropping frame",
            );
        }
    }
}

async fn accept_loop(listener: UnixListener, clients: Arc<Mutex<Vec<UnixStream>>>) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                debug!(target: "deliberate::broadcast", "client connected");
                clients.lock().await.push(stream);
            }
            Err(e) => {
                warn!(target: "deliberate::broadcast", "accept failed: {e}");
                // Avoid a tight spin if the listener is in a degenerate
                // state — back off and retry.
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
                warn!(target: "deliberate::broadcast", "frame encode failed: {e}");
                continue;
            }
        };
        line.push('\n');
        let bytes = line.as_bytes();

        let mut guard = clients.lock().await;
        let taken = std::mem::take(&mut *guard);
        let mut survivors: Vec<UnixStream> = Vec::with_capacity(taken.len());
        for mut stream in taken {
            match stream.write_all(bytes).await {
                Ok(()) => survivors.push(stream),
                Err(e) => {
                    debug!(
                        target: "deliberate::broadcast",
                        "client write failed ({e}); dropping",
                    );
                }
            }
        }
        *guard = survivors;
    }
}
