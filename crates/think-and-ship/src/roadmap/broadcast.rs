//! Best-effort, fire-and-forget broadcast of roadmap mutations.
//!
//! Wire format: one newline-delimited JSON object per mutation with a
//! `family: "roadmap"` discriminator flattened on top of the typed
//! [`RoadmapFrame`] payload. The socket + fan-out tasks live in
//! [`crate::infra::broadcast`]; this is the typed view the roadmap engine
//! emits through, mirroring `crate::ship::broadcast`.

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::infra::{Broadcaster as EngineBroadcaster, Family};
use crate::roadmap::domain::Chunk;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoadmapFrame {
    ChunkAdded {
        chunk: Chunk,
    },
    /// A non-terminal change (status/update/reprioritize/link).
    ChunkChanged {
        chunk: Chunk,
    },
    /// Terminal: the chunk shipped. Triggers a session commit when shared.
    ChunkCompleted {
        chunk: Chunk,
    },
    /// Terminal: the chunk was retired. Triggers a session commit when shared.
    ChunkObsoleted {
        chunk: Chunk,
    },
    RefreshRecorded {
        summary: String,
        think_steps: Vec<u32>,
    },
}

impl RoadmapFrame {
    /// `(kind, closes_session)` for the git-native record mapping.
    /// `kind` is the Agent Trace record discriminator; `closes_session` marks
    /// the chunk-lifecycle close events that trigger a commit.
    pub fn record_meta(&self) -> (&'static str, bool) {
        match self {
            Self::ChunkAdded { .. } | Self::ChunkChanged { .. } => ("chunk", false),
            Self::ChunkCompleted { .. } | Self::ChunkObsoleted { .. } => ("chunk", true),
            Self::RefreshRecorded { .. } => ("refresh", false),
        }
    }
}

/// Cheaply cloneable handle wrapping the shared [`EngineBroadcaster`], tagging
/// every emitted frame with `family: "roadmap"`.
#[derive(Clone)]
pub struct Broadcaster {
    inner: EngineBroadcaster,
}

impl Broadcaster {
    /// Wrap an existing engine broadcaster (already bound to the shared socket)
    /// so the roadmap family can emit through it without binding a second
    /// listener. Wired by `cli::build_unified`.
    pub fn from_engine(inner: EngineBroadcaster) -> Self {
        Self { inner }
    }

    pub fn emit(&self, frame: &RoadmapFrame) {
        if let Err(e) = self.inner.emit(Family::Roadmap, frame) {
            warn!(
                target: "think_and_ship::roadmap::broadcast",
                "dropping broadcast frame: {e}",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadmap::domain::ChunkStatus;

    fn chunk() -> Chunk {
        Chunk {
            tier: None,
            id: "a".into(),
            title: "A".into(),
            name: crate::roadmap::name::derive("a"),
            status: ChunkStatus::Done,
            priority: 1,
            description: String::new(),
            content: None,
            group: None,
            notes: String::new(),
            acceptance: vec![],
            deps: vec![],
            cross_refs: vec![],
            shared: true,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            project_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn frame_serializes_with_type_tag() {
        let f = RoadmapFrame::ChunkAdded { chunk: chunk() };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["type"], "chunk_added");
        assert_eq!(v["chunk"]["id"], "a");
    }

    #[test]
    fn record_meta_marks_terminal_events() {
        assert_eq!(
            RoadmapFrame::ChunkAdded { chunk: chunk() }.record_meta(),
            ("chunk", false)
        );
        assert_eq!(
            RoadmapFrame::ChunkCompleted { chunk: chunk() }.record_meta(),
            ("chunk", true)
        );
        assert_eq!(
            RoadmapFrame::ChunkObsoleted { chunk: chunk() }.record_meta(),
            ("chunk", true)
        );
        assert_eq!(
            RoadmapFrame::RefreshRecorded {
                summary: "s".into(),
                think_steps: vec![]
            }
            .record_meta(),
            ("refresh", false)
        );
    }
}
