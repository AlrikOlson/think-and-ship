//! Filesystem watcher. Tails `<data_dir>/sessions/*.json` and re-loads
//! any file that changes, emitting a snapshot event per change. Uses
//! [`notify-debouncer-full`] so a rapid burst of writes (atomic
//! tmp+rename produces several events) collapses into a single update.
//!
//! Design: the notify receiver is `!Sync + !Clone` and must be owned by
//! a single thread. We pin the receiver inside a `spawn_blocking` task
//! and forward events into a tokio channel that the orchestrator reads.

use std::path::{Path, PathBuf};
use std::time::Duration;

use deliberate_mcp::persistence::{Persistence, read_history};
use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::new_debouncer;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::SourceEvent;

pub async fn run(data_dir: PathBuf, tx: mpsc::UnboundedSender<SourceEvent>) {
    let sessions_dir = data_dir.join("sessions");
    if let Err(e) = std::fs::create_dir_all(&sessions_dir) {
        warn!(
            "sessions dir {} could not be created: {e}",
            sessions_dir.display()
        );
        return;
    }

    // Bridge: notify-debouncer's `std::sync::mpsc::Receiver` lives inside
    // a blocking task that forwards every batch into the tokio channel.
    let (bridge_tx, mut bridge_rx) = mpsc::unbounded_channel::<Vec<PathBuf>>();
    let watch_dir = sessions_dir.clone();
    tokio::task::spawn_blocking(move || run_notify_loop(watch_dir, bridge_tx));

    let default_stem = Persistence::default_stem();

    while let Some(paths) = bridge_rx.recv().await {
        for path in paths {
            if !is_session_json(&path) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let session_id = if stem == default_stem {
                None
            } else {
                Some(stem.to_string())
            };
            match read_history(&path) {
                Some(history) => {
                    debug!("fs reload: {}", path.display());
                    if tx
                        .send(SourceEvent::Snapshot {
                            session_id,
                            history,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                None => {
                    // Empty file mid-write or removed entirely — ignore.
                }
            }
        }
    }
}

fn run_notify_loop(sessions_dir: PathBuf, bridge_tx: mpsc::UnboundedSender<Vec<PathBuf>>) {
    let (notify_tx, notify_rx) = std::sync::mpsc::channel();
    let mut debouncer = match new_debouncer(Duration::from_millis(200), None, notify_tx) {
        Ok(d) => d,
        Err(e) => {
            warn!("could not start fs debouncer: {e}");
            return;
        }
    };
    if let Err(e) = debouncer.watch(&sessions_dir, RecursiveMode::NonRecursive) {
        warn!("could not watch {}: {e}", sessions_dir.display());
        return;
    }

    loop {
        match notify_rx.recv() {
            Ok(Ok(events)) => {
                let mut paths: Vec<PathBuf> = Vec::new();
                for event in events {
                    if !matches!(
                        event.kind,
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                    ) {
                        continue;
                    }
                    for p in &event.paths {
                        paths.push(p.clone());
                    }
                }
                if !paths.is_empty() {
                    if bridge_tx.send(paths).is_err() {
                        return;
                    }
                }
            }
            Ok(Err(errs)) => {
                for e in errs {
                    warn!("fs watcher error: {e}");
                }
            }
            Err(_disconnected) => return,
        }
    }
}

fn is_session_json(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("json")
        && path.file_name().and_then(|n| n.to_str()) != Some(".json")
        && !path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(".tmp.json") || n.ends_with(".json.tmp"))
            .unwrap_or(false)
}
