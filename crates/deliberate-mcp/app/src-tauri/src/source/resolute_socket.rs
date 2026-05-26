use std::path::PathBuf;
use std::time::Duration;

use resolute_mcp::broadcast::BroadcastFrame;
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
                info!("resolute broadcast connected: {}", path.display());
                if tx.send(SourceEvent::ResoluteSocketConnected).is_err() {
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
                            match serde_json::from_str::<BroadcastFrame>(&line) {
                                Ok(frame) => {
                                    if tx.send(SourceEvent::ResoluteFrame(frame)).is_err() {
                                        return;
                                    }
                                }
                                Err(e) => {
                                    warn!("dropped malformed resolute frame: {e}; line={line}");
                                }
                            }
                        }
                        Ok(None) => {
                            debug!("resolute broadcast peer closed");
                            break;
                        }
                        Err(e) => {
                            warn!("resolute broadcast read failed: {e}");
                            break;
                        }
                    }
                }
                if tx.send(SourceEvent::ResoluteSocketDisconnected).is_err() {
                    return;
                }
            }
            Err(e) => {
                debug!("resolute broadcast {} not available: {e}", path.display());
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}
