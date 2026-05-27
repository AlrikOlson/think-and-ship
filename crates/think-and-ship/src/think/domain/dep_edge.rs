//! Step-to-step dependency edges with optional relation labels.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A dependency on an earlier step. Accepts either a bare integer (the
/// step number, with no relation label — preserves the pre-iteration-I
/// shape) or a structured form with an optional `relation`. The relation,
/// when present, must be one of "supports", "refutes", or "depends_on";
/// any other value is accepted into the schema but treated as unlabeled
/// (the engine's allowlist normalizes at use sites, not at the type).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum DepEdge {
    Bare(u32),
    Tagged {
        step: u32,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        relation: Option<String>,
    },
}

impl DepEdge {
    pub fn step(&self) -> u32 {
        match self {
            DepEdge::Bare(n) => *n,
            DepEdge::Tagged { step, .. } => *step,
        }
    }

    pub fn relation(&self) -> Option<&str> {
        match self {
            DepEdge::Bare(_) => None,
            DepEdge::Tagged { relation, .. } => relation.as_deref(),
        }
    }
}

impl From<u32> for DepEdge {
    fn from(n: u32) -> Self {
        DepEdge::Bare(n)
    }
}
