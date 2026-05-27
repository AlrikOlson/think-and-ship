//! Best-effort, fire-and-forget broadcast of execution-trace mutations.
//!
//! Wire format: one newline-delimited JSON object per mutation, with a
//! `family: "ship"` discriminator flattened on top of the typed
//! [`BroadcastFrame`] payload. The actual socket and fan-out tasks live
//! in [`crate::engine::broadcast`]; this module is the typed view the
//! execution engine emits through.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::engine::{Broadcaster as EngineBroadcaster, Family};
use crate::ship::domain::action::Action;
use crate::ship::domain::check::Check;
use crate::ship::domain::objective::Objective;

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

/// Cheaply cloneable handle that wraps the shared [`engine::Broadcaster`]
/// and tags every emitted frame with `family: "ship"`.
#[derive(Clone)]
pub struct Broadcaster {
    inner: EngineBroadcaster,
}

impl Broadcaster {
    pub fn spawn(path: PathBuf) -> Option<Self> {
        EngineBroadcaster::spawn(path).map(|inner| Self { inner })
    }

    /// Wrap an existing [`EngineBroadcaster`] (already spawned at the
    /// shared socket) so this family can emit through it without binding
    /// a second listener.
    pub fn from_engine(inner: EngineBroadcaster) -> Self {
        Self { inner }
    }

    pub fn emit(&self, frame: BroadcastFrame) {
        if let Err(e) = self.inner.emit(Family::Ship, &frame) {
            warn!(
                target: "think_and_ship::ship::broadcast",
                "dropping broadcast frame: {e}",
            );
        }
    }
}
