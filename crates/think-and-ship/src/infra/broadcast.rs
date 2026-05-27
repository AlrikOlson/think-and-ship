//! NDJSON-over-Unix-socket fan-out, shared by both tool families.
//!
//! Every emitted frame carries a `family` tag so a single viewer can
//! interleave `think_*` and `ship_*` events on one timeline without
//! maintaining two socket readers.
//!
//! Construct with [`Broadcaster::spawn`]; missing tokio runtime or a bind
//! failure returns `None` and the server keeps running unobserved.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

/// Identifies which family emitted a frame. Wire form is the lowercase
/// name (`"think"` / `"ship"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Think,
    Ship,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Think => "think",
            Self::Ship => "ship",
        }
    }
}

#[derive(Clone)]
pub struct Broadcaster {
    tx: mpsc::UnboundedSender<String>,
}

impl Broadcaster {
    /// Bind a Unix socket at `path` and start the accept + fan-out tasks.
    /// Returns `None` if no tokio runtime is active or the socket can't
    /// be bound — callers should treat that as "run without broadcast".
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
                warn!(target: "think_and_ship::broadcast", "could not bind {}: {e}", path.display());
                return None;
            }
        };

        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));

        tokio::spawn(accept_loop(listener, Arc::clone(&clients)));
        tokio::spawn(fanout_loop(rx, clients));

        info!(target: "think_and_ship::broadcast", "listening at {}", path.display());
        Some(Self { tx })
    }

    /// Encode a family-tagged frame: `{"family": "<f>", ...payload}`.
    ///
    /// `payload` must serialize to a JSON object so the `family` field can
    /// be flattened onto it. Returns `Err` if encoding fails; otherwise
    /// the frame is queued for fan-out and the caller does not block.
    pub fn emit<T: Serialize>(&self, family: Family, payload: &T) -> Result<(), EmitError> {
        let mut value = serde_json::to_value(payload).map_err(EmitError::Encode)?;
        let obj = value.as_object_mut().ok_or(EmitError::PayloadNotObject)?;
        obj.insert(
            "family".to_string(),
            serde_json::Value::String(family.as_str().to_string()),
        );
        let line = serde_json::to_string(&value).map_err(EmitError::Encode)?;
        self.tx.send(line).map_err(|_| EmitError::Closed)
    }
}

#[derive(Debug)]
pub enum EmitError {
    Encode(serde_json::Error),
    PayloadNotObject,
    Closed,
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "could not encode broadcast frame: {e}"),
            Self::PayloadNotObject => write!(f, "broadcast payload must serialize to a JSON object"),
            Self::Closed => write!(f, "broadcaster channel is closed"),
        }
    }
}

impl std::error::Error for EmitError {}

async fn accept_loop(listener: UnixListener, clients: Arc<Mutex<Vec<UnixStream>>>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                debug!(target: "think_and_ship::broadcast", "client connected");
                clients.lock().await.push(stream);
            }
            Err(e) => {
                warn!(target: "think_and_ship::broadcast", "accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn fanout_loop(mut rx: mpsc::UnboundedReceiver<String>, clients: Arc<Mutex<Vec<UnixStream>>>) {
    while let Some(mut line) = rx.recv().await {
        line.push('\n');
        let bytes = line.as_bytes();
        let mut guard = clients.lock().await;
        let taken = std::mem::take(&mut *guard);
        let mut survivors = Vec::with_capacity(taken.len());
        for mut stream in taken {
            match stream.write_all(bytes).await {
                Ok(()) => survivors.push(stream),
                Err(_) => debug!(target: "think_and_ship::broadcast", "client disconnected"),
            }
        }
        *guard = survivors;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[derive(Serialize)]
    struct Sample {
        kind: &'static str,
        n: u32,
    }

    #[derive(Serialize)]
    struct NotAnObject(u32);

    #[tokio::test]
    async fn spawn_returns_some_with_valid_path() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock);
        assert!(b.is_some());
    }

    #[tokio::test]
    async fn emit_flattens_family_onto_payload() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock.clone()).expect("spawn");

        let stream = UnixStream::connect(&sock).await.unwrap();
        let mut reader = BufReader::new(stream);

        b.emit(Family::Think, &Sample { kind: "step", n: 7 }).unwrap();

        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();

        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["family"], "think");
        assert_eq!(v["kind"], "step");
        assert_eq!(v["n"], 7);
    }

    #[tokio::test]
    async fn non_object_payload_returns_error() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock).expect("spawn");
        let err = b.emit(Family::Ship, &NotAnObject(1)).unwrap_err();
        assert!(matches!(err, EmitError::PayloadNotObject));
    }
}
