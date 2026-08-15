//! Forgiving serde coercions for MCP tool arguments.
//!
//! Agents routinely send a single string where a tool expects a string array
//! (e.g. `constraints: "surgical changes only"` instead of `["…"]`), or a
//! named level (`"high"`) where a tool expects an integer. The strict serde
//! default rejects these with a `-32602 invalid type` JSON-RPC error — and in
//! Claude Code that error cascade-cancels *every sibling tool call in the same
//! parallel batch* (anthropics/claude-code#22264), stalling the whole turn.
//!
//! This module is the single home (SRP) for the "never reject an argument"
//! discipline: every helper here returns a value or a sane fallback and is
//! **infallible by construction** — it never produces a `D::Error` for a
//! shape mismatch. Arg structs pair these with `#[serde(default)]` so an
//! absent field is also fine. The net guarantee: argument deserialization for
//! an MCP tool cannot emit `-32602`, so it can never be the errored sibling
//! that triggers the cascade. Required-ness is enforced *inside the handler*
//! as a soft, non-error result (see `infra::tool_result`), not at this layer.

use serde::{Deserialize, Deserializer};

/// `#[serde(deserialize_with = "infra::coerce::string_or_seq")]` — accept a
/// string array, a single string (wrapped into a one-element list), `null`
/// (empty), or a scalar (stringified). Pair with `#[serde(default)]` so an
/// absent field still yields an empty list.
pub fn string_or_seq<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(coerce_string_vec(value))
}

/// Normalize a JSON value into a `Vec<String>` as forgivingly as is sane.
fn coerce_string_vec(value: serde_json::Value) -> Vec<String> {
    use serde_json::Value;
    match value {
        Value::Null => Vec::new(),
        Value::String(s) => {
            if s.trim().is_empty() {
                Vec::new()
            } else {
                vec![s]
            }
        }
        Value::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        other => vec![other.to_string()],
    }
}

/// Coerce a JSON value into a `u32` as forgivingly as is sane, never failing.
/// Accepts an integer, a float (truncated), or a numeric string. Anything
/// unparseable — including a named level the caller didn't map — falls back to
/// `0`, which handlers treat as "unset" and validate themselves.
fn coerce_u32(value: &serde_json::Value) -> u32 {
    use serde_json::Value;
    match value {
        Value::Number(n) => {
            let raw = n
                .as_u64()
                .or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
                .unwrap_or(0);
            u32::try_from(raw).unwrap_or(u32::MAX)
        }
        Value::String(s) => s.trim().parse::<u32>().unwrap_or(0),
        _ => 0,
    }
}

/// `#[serde(deserialize_with = "infra::coerce::lenient_u32")]` — accept an
/// integer, a numeric string, or anything else (→ `0`). Never errors. Pair
/// with `#[serde(default)]`. Use for count/number fields where the handler
/// validates the `0` case.
pub fn lenient_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(coerce_u32(&value))
}

/// `#[serde(deserialize_with = "infra::coerce::lenient_bool")]` — accept a
/// bool, the strings "true"/"false"/"1"/"0"/"yes"/"no" (case-insensitive), or
/// a number (non-zero → true). Anything else → `false`. Never errors.
pub fn lenient_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_json::Value;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Bool(b) => b,
        Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "y" | "on"
        ),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        _ => false,
    })
}

/// `#[serde(deserialize_with = "infra::coerce::lenient_opt_bool")]` — like
/// [`lenient_bool`] but yields `Option<bool>`: `null`/absent → `None`, every
/// other shape coerces to `Some(bool)`. Never errors. For tri-state flags
/// where "unset" is meaningful (e.g. pin defaults to true when omitted).
pub fn lenient_opt_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_json::Value;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Null => None,
        Value::Bool(b) => Some(b),
        Value::String(s) => Some(matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "y" | "on"
        )),
        Value::Number(n) => Some(n.as_f64().map(|f| f != 0.0).unwrap_or(false)),
        _ => None,
    })
}

/// `#[serde(deserialize_with = "infra::coerce::lenient_opt_u32")]` — like
/// [`lenient_u32`] but yields `Option<u32>`: `null`/absent → `None`, every
/// other shape coerces to `Some(u32)` (unparseable → `Some(0)`). Never errors.
pub fn lenient_opt_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => None,
        other => Some(coerce_u32(&other)),
    })
}

/// Map a named priority level to its numeric weight (lower sorts earlier),
/// or parse a numeric string. Returns `None` for anything unrecognized so the
/// caller decides the fallback. Shared by the roadmap priority deserializers.
pub fn parse_priority_level(s: &str) -> Option<u32> {
    match s.trim().to_ascii_lowercase().as_str() {
        "critical" | "crit" => Some(100),
        "high" => Some(200),
        "medium" | "med" | "normal" => Some(300),
        "low" => Some(400),
        other => other.parse::<u32>().ok(),
    }
}

/// `#[serde(deserialize_with = "infra::coerce::optional_priority")]` — the
/// [`priority`] coercion for a field that may be absent. An explicit `null` and
/// an unrecognized value both mean "leave it alone" rather than 0, because on a
/// patch tool a bogus priority must not silently rewrite the chunk's place.
pub fn optional_priority<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_json::Value;
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(coerce_u32(&Value::Number(n))),
        Some(Value::String(s)) => parse_priority_level(&s),
        Some(_) => None,
    })
}

/// The band a stored priority falls in — the inverse of
/// [`parse_priority_level`], and its neighbour on purpose: two directions of
/// one vocabulary that must not drift apart.
///
/// Names went IN (`roadmap_add_chunk` has always taken `critical`/`high`/…) and
/// only integers came OUT, so every human-facing surface printed magic numbers.
/// Bands are inclusive upper bounds, and `later` covers everything past `low`
/// rather than pretending a 552 is "low".
pub fn priority_band(priority: u32) -> &'static str {
    match priority {
        0..=100 => "critical",
        101..=200 => "high",
        201..=300 => "medium",
        301..=400 => "low",
        _ => "later",
    }
}

/// `#[serde(deserialize_with = "infra::coerce::priority")]` — forgiving
/// priority: accept a raw integer, a numeric string, or a named level
/// (`critical`/`high`/`medium`/`low` → 100/200/300/400). Unrecognized → `0`.
/// Never errors (unlike a strict enum, which would emit `-32602`).
pub fn priority<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_json::Value;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Number(n) => coerce_u32(&Value::Number(n)),
        Value::String(s) => parse_priority_level(&s).unwrap_or(0),
        _ => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "string_or_seq")]
        items: Vec<String>,
    }

    fn items(json: &str) -> Vec<String> {
        serde_json::from_str::<Holder>(json).unwrap().items
    }

    #[test]
    fn accepts_a_single_string() {
        assert_eq!(items(r#"{"items":"only one"}"#), vec!["only one"]);
    }

    #[test]
    fn accepts_an_array() {
        assert_eq!(items(r#"{"items":["a","b"]}"#), vec!["a", "b"]);
    }

    #[test]
    fn absent_null_and_empty_string_yield_empty() {
        assert!(items(r#"{}"#).is_empty());
        assert!(items(r#"{"items":null}"#).is_empty());
        assert!(items(r#"{"items":"  "}"#).is_empty());
    }

    #[test]
    fn stringifies_scalars_in_an_array() {
        assert_eq!(items(r#"{"items":[1,true]}"#), vec!["1", "true"]);
    }

    // ── infallible scalar coercions ──────────────────────────────────

    #[derive(Deserialize)]
    struct Nums {
        #[serde(default, deserialize_with = "lenient_u32")]
        n: u32,
        #[serde(default, deserialize_with = "priority")]
        p: u32,
        #[serde(default, deserialize_with = "lenient_bool")]
        b: bool,
        #[serde(default, deserialize_with = "lenient_opt_bool")]
        ob: Option<bool>,
        #[serde(default, deserialize_with = "lenient_opt_u32")]
        on: Option<u32>,
    }

    fn nums(json: &str) -> Nums {
        serde_json::from_str(json).expect("forgiving args never reject")
    }

    #[test]
    fn lenient_u32_accepts_int_string_and_falls_back() {
        assert_eq!(nums(r#"{"n":5}"#).n, 5);
        assert_eq!(nums(r#"{"n":"7"}"#).n, 7);
        assert_eq!(nums(r#"{"n":"nonsense"}"#).n, 0);
        assert_eq!(nums(r#"{"n":true}"#).n, 0);
        assert_eq!(nums(r#"{}"#).n, 0);
    }

    #[test]
    fn priority_accepts_named_or_numeric() {
        assert_eq!(nums(r#"{"p":"high"}"#).p, 200);
        assert_eq!(nums(r#"{"p":"CRITICAL"}"#).p, 100);
        assert_eq!(nums(r#"{"p":250}"#).p, 250);
        assert_eq!(nums(r#"{"p":"42"}"#).p, 42);
        assert_eq!(nums(r#"{"p":"urgent"}"#).p, 0); // unrecognized → 0, no error
    }

    /// Every name the parser accepts must round-trip back to itself, or the two
    /// directions of the vocabulary have drifted and a chunk added as "high"
    /// would display as something else.
    #[test]
    fn priority_bands_round_trip_their_own_names() {
        for name in ["critical", "high", "medium", "low"] {
            let stored = parse_priority_level(name).expect("a known level");
            assert_eq!(
                priority_band(stored),
                name,
                "{name} parses to {stored}, which must band back to {name}"
            );
        }
    }

    #[test]
    fn priority_bands_cover_the_whole_range_without_lying() {
        // Boundaries are inclusive upper bounds.
        assert_eq!(priority_band(0), "critical");
        assert_eq!(priority_band(100), "critical");
        assert_eq!(priority_band(101), "high");
        assert_eq!(priority_band(300), "medium");
        assert_eq!(priority_band(301), "low");
        // Past `low`, say so rather than calling a 552 "low".
        assert_eq!(priority_band(401), "later");
        assert_eq!(priority_band(u32::MAX), "later");
    }

    #[test]
    fn lenient_bool_accepts_many_shapes() {
        assert!(nums(r#"{"b":true}"#).b);
        assert!(nums(r#"{"b":"yes"}"#).b);
        assert!(nums(r#"{"b":"1"}"#).b);
        assert!(nums(r#"{"b":1}"#).b);
        assert!(!nums(r#"{"b":"no"}"#).b);
        assert!(!nums(r#"{"b":0}"#).b);
        assert!(!nums(r#"{}"#).b);
    }

    #[test]
    fn opt_helpers_distinguish_null_from_value() {
        assert_eq!(nums(r#"{}"#).ob, None);
        assert_eq!(nums(r#"{"ob":null}"#).ob, None);
        assert_eq!(nums(r#"{"ob":"true"}"#).ob, Some(true));
        assert_eq!(nums(r#"{}"#).on, None);
        assert_eq!(nums(r#"{"on":"9"}"#).on, Some(9));
        assert_eq!(nums(r#"{"on":"x"}"#).on, Some(0));
    }
}
