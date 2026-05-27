//! Unified broadcast-socket consumer. Connects to the merged MCP
//! server's NDJSON Unix-domain socket, decodes each frame's `family`
//! tag, and forwards either a `DeliberateBroadcastFrame` or a
//! `ResoluteBroadcastFrame` to the orchestrator. Disconnect = sleep +
//! reconnect with exponential backoff capped at 5 seconds.

use std::path::PathBuf;
use std::time::Duration;

use deliberate_mcp::broadcast::BroadcastFrame as DeliberateBroadcastFrame;
use resolute_mcp::broadcast::BroadcastFrame as ResoluteBroadcastFrame;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::SourceEvent;

pub async fn run(path: PathBuf, tx: mpsc::UnboundedSender<SourceEvent>) {
    let mut backoff = Duration::from_millis(250);
    let max_backoff = Duration::from_secs(5);

    loop {
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                info!("broadcast socket connected: {}", path.display());
                if tx.send(SourceEvent::SocketConnected).is_err() {
                    return;
                }
                backoff = Duration::from_millis(250);
                let mut reader = BufReader::new(stream).lines();
                loop {
                    match reader.next_line().await {
                        Ok(Some(line)) => {
                            if line.trim().is_empty() {
                                continue;
                            }
                            if !dispatch_frame(&line, &tx) {
                                return; // channel closed
                            }
                        }
                        Ok(None) => {
                            debug!("broadcast socket EOF; reconnecting");
                            break;
                        }
                        Err(e) => {
                            warn!("broadcast socket read error: {e}; reconnecting");
                            break;
                        }
                    }
                }
                let _ = tx.send(SourceEvent::SocketDisconnected);
            }
            Err(e) => {
                debug!(
                    "broadcast socket not available ({e}); retry in {:?}",
                    backoff
                );
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}

/// Parse a single NDJSON line and dispatch it to the orchestrator
/// channel based on its `family` tag. Returns `false` only when the
/// channel is closed (so the caller can return early).
fn dispatch_frame(line: &str, tx: &mpsc::UnboundedSender<SourceEvent>) -> bool {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            warn!("broadcast: malformed JSON, dropping: {e}");
            return true;
        }
    };

    let family = value
        .get("family")
        .and_then(Value::as_str)
        .unwrap_or("think"); // pre-tag frames from v0.1.x flat sockets default to think

    match family {
        "think" => match serde_json::from_value::<DeliberateBroadcastFrame>(value) {
            Ok(frame) => tx.send(SourceEvent::Frame(frame)).is_ok(),
            Err(e) => {
                warn!("broadcast: family=think frame failed to decode: {e}");
                true
            }
        },
        "ship" => match serde_json::from_value::<ResoluteBroadcastFrame>(value) {
            Ok(frame) => tx.send(SourceEvent::ResoluteFrame(frame)).is_ok(),
            Err(e) => {
                warn!("broadcast: family=ship frame failed to decode: {e}");
                true
            }
        },
        other => {
            warn!("broadcast: unknown family {other:?}, dropping frame");
            true
        }
    }
}
