//! Typed cross-references between `think_*` and `ship_*` entities.
//!
//! In-process, all cross-references are values of [`CrossRef`]. The wire
//! contract used by tool inputs and persisted traces is a `prefix:value`
//! string preserved for backward compatibility with v0.1.x clients.

use std::fmt;

use serde::{Deserialize, Serialize};

pub type StepNumber = u32;
pub type TaskId = String;
pub type ActionId = u32;
pub type CheckName = String;
pub type ChunkId = String;
pub type SignalId = String;
/// A tracker provider key, e.g. `"github"`, `"linear"`, `"jira"`. Normalized to
/// lowercase — provider keys are ours to name, so they may be case-folded.
pub type ProviderId = String;
/// An id minted by an external tracker. Case- and punctuation-preserving: a
/// GitHub GraphQL node id is base64 and a Jira key is uppercase, so unlike the
/// provider this is never folded.
pub type ExternalId = String;

/// Separates provider from external id inside an `ext:` cross-ref value.
const EXTERNAL_SEP: char = '/';

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum CrossRef {
    ThinkStep(StepNumber),
    ShipTask(TaskId),
    ShipAction(ActionId),
    ShipCheck(CheckName),
    /// A roadmap chunk: the long-horizon item a ship objective
    /// realizes and a think step shapes. Wire form `chunk:<slug>`.
    RoadmapChunk(ChunkId),
    /// A stakeholder signal: the opportunity a roadmap chunk was
    /// promoted from. Wire form `signal:<id>`. Promotion writes this onto the
    /// chunk and the reciprocal `chunk:<slug>` onto the signal.
    SignalRef(SignalId),
    /// A work item in an EXTERNAL tracker — the twin a roadmap chunk was
    /// projected onto (tracker-port-seam). Wire form
    /// `ext:<provider>/<external_id>`, e.g. `ext:github/1234`,
    /// `ext:linear/ENG-42`, `ext:jira/PROJ-7`.
    ///
    /// This is the one cross-ref whose referent lives outside the system, which
    /// is exactly why it belongs here rather than in a provider module: the
    /// provenance graph gains an edge to a tracker without any core type
    /// learning what a tracker *is*. Adding a provider adds a string, not a
    /// variant.
    ///
    /// Two-part because a bare external id is meaningless — issue `1234` exists
    /// in every tracker on earth. `provider` is normalized to lowercase;
    /// `external_id` is preserved byte-for-byte (see [`ExternalId`]).
    External {
        provider: ProviderId,
        external_id: ExternalId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingDelimiter,
    UnknownPrefix(String),
    InvalidNumber(String),
    EmptyValue,
    /// An `ext:` ref whose value has no `provider/external_id` split.
    MissingProviderSeparator,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDelimiter => write!(f, "cross-ref string must contain ':'"),
            Self::UnknownPrefix(p) => write!(
                f,
                "unknown cross-ref prefix '{p}' (expected think|task|action|check|chunk|signal|ext)"
            ),
            Self::InvalidNumber(s) => write!(f, "expected integer, got '{s}'"),
            Self::EmptyValue => write!(f, "cross-ref value is empty"),
            Self::MissingProviderSeparator => write!(
                f,
                "external cross-ref must be 'ext:<provider>{EXTERNAL_SEP}<external_id>'"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

impl CrossRef {
    /// Build an [`CrossRef::External`] with the provider normalized the same way
    /// parsing normalizes it, so a hand-built ref and a parsed one are equal.
    /// Prefer this over the struct literal.
    #[must_use]
    pub fn external(provider: &str, external_id: &str) -> Self {
        Self::External {
            provider: provider.trim().to_ascii_lowercase(),
            external_id: external_id.trim().to_string(),
        }
    }

    /// Serialize to the wire string form: `think:42`, `task:auth-refactor`,
    /// `action:5`, `check:cargo-test`, `chunk:phase-26c`,
    /// `ext:github/1234`.
    pub fn to_wire(&self) -> String {
        match self {
            Self::ThinkStep(n) => format!("think:{n}"),
            Self::ShipTask(id) => format!("task:{id}"),
            Self::ShipAction(n) => format!("action:{n}"),
            Self::ShipCheck(name) => format!("check:{name}"),
            Self::RoadmapChunk(id) => format!("chunk:{id}"),
            Self::SignalRef(id) => format!("signal:{id}"),
            Self::External {
                provider,
                external_id,
            } => format!("ext:{provider}{EXTERNAL_SEP}{external_id}"),
        }
    }

    /// Parse from the wire string form.
    pub fn from_wire(s: &str) -> Result<Self, ParseError> {
        let s = s.trim();
        let (prefix, value) = s.split_once(':').ok_or(ParseError::MissingDelimiter)?;
        let value = value.trim();
        if value.is_empty() {
            return Err(ParseError::EmptyValue);
        }
        match prefix.trim().to_ascii_lowercase().as_str() {
            "think" | "step" => value
                .parse::<u32>()
                .map(Self::ThinkStep)
                .map_err(|_| ParseError::InvalidNumber(value.to_string())),
            "task" => Ok(Self::ShipTask(value.to_string())),
            "action" => value
                .parse::<u32>()
                .map(Self::ShipAction)
                .map_err(|_| ParseError::InvalidNumber(value.to_string())),
            "check" => Ok(Self::ShipCheck(value.to_string())),
            "chunk" => Ok(Self::RoadmapChunk(value.to_string())),
            "signal" => Ok(Self::SignalRef(value.to_string())),
            // Split on the FIRST separator only: everything after it is the
            // external id, so `ext:github/owner/repo#12` keeps its slashes and a
            // base64 GraphQL node id survives intact.
            "ext" | "external" => {
                let (provider, external_id) = value
                    .split_once(EXTERNAL_SEP)
                    .ok_or(ParseError::MissingProviderSeparator)?;
                // Normalize through the constructor so a parsed ref and a
                // hand-built one are byte-identical, then reject empty halves.
                match Self::external(provider, external_id) {
                    Self::External {
                        provider,
                        external_id,
                    } if provider.is_empty() || external_id.is_empty() => {
                        Err(ParseError::EmptyValue)
                    }
                    r => Ok(r),
                }
            }
            other => Err(ParseError::UnknownPrefix(other.to_string())),
        }
    }
}

impl fmt::Display for CrossRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire())
    }
}

impl From<CrossRef> for String {
    fn from(r: CrossRef) -> Self {
        r.to_wire()
    }
}

impl TryFrom<String> for CrossRef {
    type Error = ParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::from_wire(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(r: CrossRef) {
        let s = r.to_wire();
        let back = CrossRef::from_wire(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn round_trip_think_step() {
        round_trip(CrossRef::ThinkStep(42));
    }

    #[test]
    fn round_trip_ship_task() {
        round_trip(CrossRef::ShipTask("auth-refactor".to_string()));
    }

    #[test]
    fn round_trip_ship_action() {
        round_trip(CrossRef::ShipAction(7));
    }

    #[test]
    fn round_trip_ship_check() {
        round_trip(CrossRef::ShipCheck("cargo-test".to_string()));
    }

    #[test]
    fn round_trip_roadmap_chunk() {
        round_trip(CrossRef::RoadmapChunk("phase-26c".to_string()));
    }

    #[test]
    fn round_trip_signal_ref() {
        round_trip(CrossRef::SignalRef("f1c9a4e2".to_string()));
    }

    #[test]
    fn signal_wire_form() {
        assert_eq!(
            CrossRef::SignalRef("abc123".to_string()).to_wire(),
            "signal:abc123"
        );
        assert_eq!(
            CrossRef::from_wire("signal:abc123").unwrap(),
            CrossRef::SignalRef("abc123".to_string())
        );
    }

    #[test]
    fn chunk_wire_form() {
        assert_eq!(
            CrossRef::RoadmapChunk("phase-1".to_string()).to_wire(),
            "chunk:phase-1"
        );
        assert_eq!(
            CrossRef::from_wire("chunk:phase-1").unwrap(),
            CrossRef::RoadmapChunk("phase-1".to_string())
        );
    }

    #[test]
    fn round_trip_external() {
        round_trip(CrossRef::external("github", "1234"));
        round_trip(CrossRef::external("linear", "ENG-42"));
        round_trip(CrossRef::external("jira", "PROJ-7"));
    }

    #[test]
    fn external_wire_form() {
        assert_eq!(
            CrossRef::external("github", "1234").to_wire(),
            "ext:github/1234"
        );
        assert_eq!(
            CrossRef::from_wire("ext:github/1234").unwrap(),
            CrossRef::external("github", "1234")
        );
        // `external` is accepted as a long-form alias, like `step` for `think`.
        assert_eq!(
            CrossRef::from_wire("external:github/1234").unwrap(),
            CrossRef::external("github", "1234")
        );
    }

    /// The asymmetry that matters: the provider key is ours to name so it folds
    /// to lowercase, but an external id belongs to the tracker and must survive
    /// byte-for-byte — GitHub GraphQL node ids are base64 and Jira keys are
    /// uppercase, so folding either would produce a ref that resolves to
    /// nothing.
    #[test]
    fn external_normalizes_provider_but_never_the_id() {
        let r = CrossRef::from_wire("ext:GitHub/MDU6SXNzdWUx").unwrap();
        assert_eq!(
            r,
            CrossRef::External {
                provider: "github".into(),
                external_id: "MDU6SXNzdWUx".into(),
            }
        );
        assert_eq!(r.to_wire(), "ext:github/MDU6SXNzdWUx");
    }

    /// Only the FIRST separator splits, so ids containing `/` or `:` survive —
    /// `owner/repo#12` is a legitimate GitHub-shaped id and base64 node ids can
    /// contain `/` too. If this regressed, every such ref would silently point
    /// at a truncated item.
    #[test]
    fn external_id_may_contain_separators() {
        let r = CrossRef::from_wire("ext:github/AlrikOlson/think-and-ship#12").unwrap();
        assert_eq!(
            r,
            CrossRef::external("github", "AlrikOlson/think-and-ship#12")
        );
        round_trip(r);

        let r = CrossRef::from_wire("ext:jira/https://x.atlassian.net/browse/AB-1").unwrap();
        assert_eq!(
            r,
            CrossRef::external("jira", "https://x.atlassian.net/browse/AB-1")
        );
        round_trip(r);
    }

    #[test]
    fn external_without_separator_rejected() {
        let e = CrossRef::from_wire("ext:github").unwrap_err();
        assert_eq!(e, ParseError::MissingProviderSeparator);
    }

    #[test]
    fn external_with_empty_half_rejected() {
        assert_eq!(
            CrossRef::from_wire("ext:/1234").unwrap_err(),
            ParseError::EmptyValue
        );
        assert_eq!(
            CrossRef::from_wire("ext:github/").unwrap_err(),
            ParseError::EmptyValue
        );
        assert_eq!(
            CrossRef::from_wire("ext:github/   ").unwrap_err(),
            ParseError::EmptyValue
        );
    }

    #[test]
    fn external_serde_round_trip() {
        let r = CrossRef::external("linear", "ENG-42");
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "\"ext:linear/ENG-42\"");
        let back: CrossRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    /// The additive guarantee: every pre-existing wire form still parses to the
    /// same variant it always did. A stored trace written before `ext:` existed
    /// must load unchanged.
    #[test]
    fn existing_wire_forms_are_unaffected() {
        for (wire, expected) in [
            ("think:42", CrossRef::ThinkStep(42)),
            ("task:auth", CrossRef::ShipTask("auth".into())),
            ("action:5", CrossRef::ShipAction(5)),
            ("check:cargo-test", CrossRef::ShipCheck("cargo-test".into())),
            ("chunk:phase-1", CrossRef::RoadmapChunk("phase-1".into())),
            ("signal:abc", CrossRef::SignalRef("abc".into())),
        ] {
            assert_eq!(CrossRef::from_wire(wire).unwrap(), expected);
            assert_eq!(expected.to_wire(), wire);
        }
    }

    #[test]
    fn legacy_step_prefix_parses() {
        let r = CrossRef::from_wire("step:9").unwrap();
        assert_eq!(r, CrossRef::ThinkStep(9));
    }

    #[test]
    fn unknown_prefix_rejected() {
        let e = CrossRef::from_wire("nope:1").unwrap_err();
        assert!(matches!(e, ParseError::UnknownPrefix(_)));
    }

    #[test]
    fn missing_delimiter_rejected() {
        let e = CrossRef::from_wire("task-without-colon").unwrap_err();
        assert_eq!(e, ParseError::MissingDelimiter);
    }

    #[test]
    fn empty_value_rejected() {
        let e = CrossRef::from_wire("task: ").unwrap_err();
        assert_eq!(e, ParseError::EmptyValue);
    }

    #[test]
    fn whitespace_tolerated() {
        let r = CrossRef::from_wire("  think:5  ").unwrap();
        assert_eq!(r, CrossRef::ThinkStep(5));
    }

    #[test]
    fn case_insensitive_prefix() {
        let r = CrossRef::from_wire("TASK:foo").unwrap();
        assert_eq!(r, CrossRef::ShipTask("foo".to_string()));
    }

    #[test]
    fn serde_round_trip() {
        let r = CrossRef::ShipTask("x".to_string());
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "\"task:x\"");
        let back: CrossRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
