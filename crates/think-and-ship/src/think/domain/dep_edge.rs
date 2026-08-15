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
        // `on`/`from` are forgiving aliases for `step` — agents reach for
        // them naturally (`{on: 48}`); accept them rather than reject.
        #[serde(alias = "on", alias = "from")]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> DepEdge {
        serde_json::from_str(json).expect("DepEdge should deserialize")
    }

    #[test]
    fn bare_integer() {
        assert_eq!(parse("5").step(), 5);
        assert_eq!(parse("5").relation(), None);
    }

    #[test]
    fn tagged_step_field() {
        let e = parse(r#"{"step":7,"relation":"supports"}"#);
        assert_eq!(e.step(), 7);
        assert_eq!(e.relation(), Some("supports"));
    }

    #[test]
    fn on_and_from_aliases_for_step() {
        // Agents naturally write {on: N} / {from: N}; both alias to `step`.
        assert_eq!(parse(r#"{"on":48}"#).step(), 48);
        assert_eq!(parse(r#"{"from":12}"#).step(), 12);
        assert_eq!(
            parse(r#"{"on":48,"relation":"depends_on"}"#).relation(),
            Some("depends_on")
        );
    }
}
