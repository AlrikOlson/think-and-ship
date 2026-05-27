//! Best-effort, fire-and-forget broadcast of reasoning-trace mutations.
//!
//! Wire format: one newline-delimited JSON object per mutation, with a
//! `family: "think"` discriminator flattened on top of the typed
//! [`BroadcastFrame`] payload. The actual socket and fan-out tasks live
//! in [`crate::infra::broadcast`]; this module is the typed view that
//! the reasoning engine emits through.
//!
//! Emission is never allowed to fail the calling MCP tool path:
//!
//! - Bind failure at startup returns `None` from [`Broadcaster::spawn`];
//!   [`ReasoningServer`] then runs without a broadcaster.
//! - Encode or channel-send failure is logged at `WARN` and discarded.
//!
//! [`ReasoningServer`]: crate::think::engine::core::ReasoningServer

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::infra::{Broadcaster as EngineBroadcaster, Family};
use crate::think::domain::{BranchStatus, DeliberateStep};

/// One mutation event. Encoded as a single JSON object with an externally
/// tagged `type` discriminant so a stream of mixed frames is unambiguous.
/// The wrapping [`Broadcaster`] flattens a `family: "think"` field onto
/// every emitted frame so a shared viewer can interleave events from
/// other tool families on one timeline.
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

/// Cheaply cloneable handle that wraps the shared [`engine::Broadcaster`]
/// and tags every emitted frame with `family: "think"`.
#[derive(Clone)]
pub struct Broadcaster {
    inner: EngineBroadcaster,
}

impl Broadcaster {
    /// Bind the shared socket at `path` (or join an existing one) and
    /// return a think-family handle. Returns `None` if no tokio runtime
    /// is active or the bind failed; in either case the server runs
    /// unaffected.
    pub fn spawn(path: PathBuf) -> Option<Self> {
        EngineBroadcaster::spawn(path).map(|inner| Self { inner })
    }

    /// Wrap an existing [`EngineBroadcaster`] (already spawned at the
    /// shared socket) so this family can emit through it without binding
    /// a second listener.
    pub fn from_engine(inner: EngineBroadcaster) -> Self {
        Self { inner }
    }

    /// Enqueue a frame for fan-out. Fire-and-forget: any encode or
    /// channel error is logged and dropped so the calling tool path is
    /// never affected.
    pub fn emit(&self, frame: BroadcastFrame) {
        if let Err(e) = self.inner.emit(Family::Think, &frame) {
            warn!(
                target: "think_and_ship::think::broadcast",
                "dropping broadcast frame: {e}",
            );
        }
    }
}
