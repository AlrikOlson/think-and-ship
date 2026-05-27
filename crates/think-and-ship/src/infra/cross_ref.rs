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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum CrossRef {
    ThinkStep(StepNumber),
    ShipTask(TaskId),
    ShipAction(ActionId),
    ShipCheck(CheckName),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingDelimiter,
    UnknownPrefix(String),
    InvalidNumber(String),
    EmptyValue,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDelimiter => write!(f, "cross-ref string must contain ':'"),
            Self::UnknownPrefix(p) => write!(
                f,
                "unknown cross-ref prefix '{p}' (expected think|task|action|check)"
            ),
            Self::InvalidNumber(s) => write!(f, "expected integer, got '{s}'"),
            Self::EmptyValue => write!(f, "cross-ref value is empty"),
        }
    }
}

impl std::error::Error for ParseError {}

impl CrossRef {
    /// Serialize to the wire string form: `think:42`, `task:auth-refactor`,
    /// `action:5`, `check:cargo-test`.
    pub fn to_wire(&self) -> String {
        match self {
            Self::ThinkStep(n) => format!("think:{n}"),
            Self::ShipTask(id) => format!("task:{id}"),
            Self::ShipAction(n) => format!("action:{n}"),
            Self::ShipCheck(name) => format!("check:{name}"),
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
