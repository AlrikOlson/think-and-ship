//! The structured body of a record — `record.content` on the wire.
//!
//! Agents historically packed everything a record had to say into one prose
//! field (`body` on a signal, `description` on a chunk), which the webapp can
//! only render as a wall of text. This module is the contract-owned answer: a
//! flat, closed set of shapes (`summary` + `facts` + `sections`) that every
//! family may carry under the uniform payload key `content`, mirroring
//! `contract/unified-record-envelope.schema.json` `$defs/StructuredContent`.
//! The prose field stays required and stays the fallback — a writer that does
//! not know this shape loses nothing, and legacy records simply omit it.
//!
//! Deliberately NOT a rich-text tree: a section cannot contain sections, so
//! every renderer is total. Limits match the schema so a payload accepted here
//! is accepted by the ajv validators the frontend and Worker generate from the
//! same file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Version of the StructuredContent shape itself, independent of the envelope
/// `schema_version`.
pub const CONTENT_VERSION: u32 = 1;

/// One key:value fact — a chip, not a sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContentFact {
    /// The dimension this fact names (e.g. "kind", "surface", "owner").
    pub label: String,
    /// The short answer. Atomic — never a sentence.
    pub value: String,
}

/// One item of a section's list. `done` marks a completed checklist item;
/// omitted means a plain (non-checklist) item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContentListItem {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
}

/// One titled slice of the record's detail: markdown prose, a checklist, or
/// both. A section never nests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContentSection {
    pub heading: String,
    /// Markdown body (paragraphs, bold/em/code, links, bullet/numbered lists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prose: Option<String>,
    /// A checklist or plain item list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<Vec<ContentListItem>>,
}

/// The structured body of a record (v1): the one-sentence `summary` every
/// card can show, the `facts` grid that would otherwise be shorthand soup in
/// a paragraph, and titled `sections` carrying the detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StructuredContent {
    /// Always [`CONTENT_VERSION`]. Serde-checked on the way in.
    pub version: u32,
    /// One plain-language sentence for a reader with no project context.
    /// No internal shorthand, no ids.
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Vec<ContentFact>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<ContentSection>>,
}

impl StructuredContent {
    /// Validate against the same limits the canonical schema declares, so a
    /// payload accepted at this seam is accepted by every generated validator
    /// downstream. Returns the first violation as a human-readable sentence —
    /// the writer sees exactly what to fix.
    pub fn validate(&self) -> Result<(), String> {
        fn len_in(name: &str, value: &str, min: usize, max: usize) -> Result<(), String> {
            let n = value.chars().count();
            if n < min {
                return Err(format!("content.{name} must not be empty"));
            }
            if n > max {
                return Err(format!("content.{name} exceeds {max} characters (got {n})"));
            }
            Ok(())
        }
        if self.version != CONTENT_VERSION {
            return Err(format!(
                "content.version must be {CONTENT_VERSION} (got {})",
                self.version
            ));
        }
        len_in("summary", &self.summary, 1, 280)?;
        if let Some(facts) = &self.facts {
            if facts.len() > 16 {
                return Err(format!(
                    "content.facts exceeds 16 items (got {})",
                    facts.len()
                ));
            }
            for fact in facts {
                len_in("facts[].label", &fact.label, 1, 48)?;
                len_in("facts[].value", &fact.value, 1, 160)?;
            }
        }
        if let Some(sections) = &self.sections {
            if sections.len() > 12 {
                return Err(format!(
                    "content.sections exceeds 12 items (got {})",
                    sections.len()
                ));
            }
            for section in sections {
                len_in("sections[].heading", &section.heading, 1, 80)?;
                if let Some(prose) = &section.prose {
                    len_in("sections[].prose", prose, 0, 4000)?;
                }
                if let Some(list) = &section.list {
                    if list.len() > 24 {
                        return Err(format!(
                            "content.sections[].list exceeds 24 items (got {})",
                            list.len()
                        ));
                    }
                    for item in list {
                        len_in("sections[].list[].text", &item.text, 1, 400)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Parse the optional `content` tool argument as every MCP seam accepts it:
/// absent or `null` is `None` (the legacy writer, losing nothing); present but
/// malformed is a soft error naming the exact problem, never a protocol-level
/// rejection that would cancel sibling calls. The arg stays a raw
/// `serde_json::Value` at the schema layer deliberately — embedding the full
/// shape in three tools' input schemas cost ~27 KB of every agent's context
/// (the model-facing-budget gates), and the compact shape line in each field
/// description steers just as well.
///
/// A JSON-encoded *string* of the object is accepted too: a client whose
/// harness only decodes parameters into the types the input schema declares
/// (Claude Code among them) delivers `"{\"version\":1,…}"` for any field the
/// schema leaves untyped, and rejecting that stranded every structured body
/// in practice. One level of decoding only — a doubly-encoded string is a
/// client bug worth surfacing, not forgiving.
pub fn parse_optional(raw: Option<serde_json::Value>) -> Result<Option<StructuredContent>, String> {
    match raw {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let decoded: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
                format!(
                    "invalid content: expected an object like \
                     {{\"version\":1,\"summary\":\"…\"}}, got a string that is \
                     not valid JSON ({e})"
                )
            })?;
            match decoded {
                serde_json::Value::String(_) => {
                    Err("invalid content: expected an object, got a doubly \
                     JSON-encoded string"
                        .to_string())
                }
                v => parse_object(v),
            }
        }
        Some(v) => parse_object(v),
    }
}

/// Deserialize + validate an already-decoded JSON value into
/// [`StructuredContent`]. Shared tail of both `parse_optional` arms.
fn parse_object(v: serde_json::Value) -> Result<Option<StructuredContent>, String> {
    let content: StructuredContent =
        serde_json::from_value(v).map_err(|e| format!("invalid content: {e}"))?;
    content.validate()?;
    Ok(Some(content))
}

/// Input schema for a `content` tool argument: a bare `{"type": "object"}`
/// (nullable), NOT the full [`StructuredContent`] shape — see the budget note
/// on [`parse_optional`]. Declaring the type is load-bearing, not cosmetic:
/// clients that decode parameters by schema (Claude Code) send a JSON-encoded
/// string for an untyped field, and the type declaration is what makes them
/// pass a real object. Use via
/// `#[schemars(schema_with = "crate::content::input_schema")]`.
pub fn input_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["object", "null"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> StructuredContent {
        StructuredContent {
            version: CONTENT_VERSION,
            summary: "A plain sentence.".to_string(),
            facts: None,
            sections: None,
        }
    }

    #[test]
    fn minimal_content_validates_and_round_trips() {
        let content = minimal();
        content.validate().unwrap();
        let wire = serde_json::to_value(&content).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({ "version": 1, "summary": "A plain sentence." })
        );
        let back: StructuredContent = serde_json::from_value(wire).unwrap();
        assert_eq!(back, content);
    }

    #[test]
    fn schema_example_shape_deserializes() {
        // The signal example in the canonical schema, record.content verbatim.
        let wire = serde_json::json!({
            "version": 1,
            "summary": "Exporting the roadmap loses nested acceptance notes under their chunks.",
            "facts": [
                { "label": "kind", "value": "bug" },
                { "label": "surface", "value": "roadmap export" }
            ],
            "sections": [
                { "heading": "What happens", "prose": "Running an export…" },
                { "heading": "How to reproduce", "list": [
                    { "text": "Add a chunk with a two-level acceptance list" },
                    { "text": "Export the roadmap and compare", "done": false }
                ] }
            ]
        });
        let content: StructuredContent = serde_json::from_value(wire.clone()).unwrap();
        content.validate().unwrap();
        assert_eq!(serde_json::to_value(&content).unwrap(), wire);
    }

    #[test]
    fn parse_optional_accepts_a_json_encoded_string() {
        // What a schema-decoding client (Claude Code) sends for an untyped
        // field: the object serialized into one JSON string.
        let wire = serde_json::json!({ "version": 1, "summary": "A plain sentence." });
        let encoded = serde_json::Value::String(wire.to_string());
        let parsed = parse_optional(Some(encoded)).unwrap().unwrap();
        assert_eq!(parsed, minimal());
    }

    #[test]
    fn parse_optional_blank_string_is_none() {
        assert_eq!(
            parse_optional(Some(serde_json::Value::String("  ".to_string()))).unwrap(),
            None
        );
    }

    #[test]
    fn parse_optional_rejects_non_json_and_double_encoding_with_the_reason() {
        let err =
            parse_optional(Some(serde_json::Value::String("not json".to_string()))).unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");

        let doubled = serde_json::Value::String(
            serde_json::Value::String("{\"version\":1}".to_string()).to_string(),
        );
        let err = parse_optional(Some(doubled)).unwrap_err();
        assert!(err.contains("doubly"), "{err}");
    }

    #[test]
    fn parse_optional_still_validates_a_decoded_string() {
        // The string decodes fine but the object inside violates the shape —
        // the validation error must be the same one a real object gets.
        let wire = serde_json::json!({ "version": 2, "summary": "A plain sentence." });
        let err = parse_optional(Some(serde_json::Value::String(wire.to_string()))).unwrap_err();
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn input_schema_declares_the_object_type() {
        let mut generator = schemars::SchemaGenerator::default();
        let schema = input_schema(&mut generator).to_value();
        assert_eq!(schema["type"], serde_json::json!(["object", "null"]));
    }

    #[test]
    fn violations_name_the_field() {
        let mut c = minimal();
        c.summary = String::new();
        assert!(c.validate().unwrap_err().contains("summary"));

        let mut c = minimal();
        c.version = 2;
        assert!(c.validate().unwrap_err().contains("version"));

        let mut c = minimal();
        c.facts = Some(vec![ContentFact {
            label: "x".repeat(49),
            value: "v".to_string(),
        }]);
        assert!(c.validate().unwrap_err().contains("label"));
    }
}
