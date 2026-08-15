//! Regression: no served tool schema carries a non-standard JSON Schema
//! `format` (schemars' uint32/uint64/…) — MCP clients warn `unknown format
//! ignored` for them on every connect (chunk: schema-format-warnings).

use serde_json::Value;
use think_and_ship::mcp::unified::UnifiedService;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::roadmap::mcp::RoadmapService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::ship::mcp::ShipService;
use think_and_ship::signal::engine::SignalEngine;
use think_and_ship::signal::mcp::SignalService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;
use think_and_ship::think::mcp::service::ThinkService;

const STANDARD_FORMATS: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uuid",
    "json-pointer",
    "relative-json-pointer",
    "regex",
];

fn build_unified() -> UnifiedService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(cfg));
    let ship = ShipService::new(ShipEngine::new("test-schema".into()));
    let roadmap = RoadmapService::new(RoadmapEngine::new("test-schema".into()));
    let signal = SignalService::new(SignalEngine::new("test-schema".into()));
    UnifiedService::new(think, ship, roadmap, signal)
}

/// Collect every `format` string in a schema, with a JSON-pointer-ish path.
fn collect_formats(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(f)) = map.get("format") {
                out.push((path.to_string(), f.clone()));
            }
            for (k, v) in map {
                collect_formats(v, &format!("{path}/{k}"), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                collect_formats(v, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn no_served_schema_carries_a_nonstandard_format() {
    let unified = build_unified();
    let mut offenders = Vec::new();
    for tool in unified.list_tools_view() {
        let mut formats = Vec::new();
        collect_formats(
            &Value::Object((*tool.input_schema).clone()),
            "input",
            &mut formats,
        );
        if let Some(schema) = &tool.output_schema {
            collect_formats(&Value::Object((**schema).clone()), "output", &mut formats);
        }
        for (path, format) in formats {
            if !STANDARD_FORMATS.contains(&format.as_str()) {
                offenders.push(format!("{}: {path} format={format}", tool.name));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "non-standard JSON Schema formats served:\n{}",
        offenders.join("\n")
    );
}

/// The cleanup must not eat the integer bounds schemars emits alongside.
#[test]
fn integer_minimum_bounds_survive_the_format_strip() {
    let unified = build_unified();
    let mut found_minimum = false;
    for tool in unified.list_tools_view() {
        let v = Value::Object((*tool.input_schema).clone());
        let mut stack = vec![&v];
        while let Some(cur) = stack.pop() {
            match cur {
                Value::Object(map) => {
                    if map.contains_key("minimum") {
                        found_minimum = true;
                    }
                    stack.extend(map.values());
                }
                Value::Array(items) => stack.extend(items.iter()),
                _ => {}
            }
        }
    }
    assert!(
        found_minimum,
        "expected at least one integer `minimum` bound to survive"
    );
}
