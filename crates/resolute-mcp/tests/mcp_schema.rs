use resolute_mcp::engine::ResoluteEngine;
use resolute_mcp::mcp::ResoluteService;

fn service() -> ResoluteService {
    let engine = ResoluteEngine::new("test-abc123".into());
    ResoluteService::new(engine)
}

#[test]
fn tools_list_has_11_tools() {
    let svc = service();
    let tools = svc.list_tools_view();
    assert_eq!(tools.len(), 11, "expected 11 tools, got {}", tools.len());
}

#[test]
fn every_tool_has_annotations() {
    let svc = service();
    for tool in svc.list_tools_view() {
        assert!(
            tool.annotations.is_some(),
            "tool '{}' missing annotations",
            tool.name
        );
    }
}

#[test]
fn every_tool_has_output_schema() {
    let svc = service();
    for tool in svc.list_tools_view() {
        assert!(
            tool.output_schema.is_some(),
            "tool '{}' missing output_schema",
            tool.name
        );
    }
}

#[test]
fn destructive_tools_are_marked() {
    let svc = service();
    for tool in svc.list_tools_view() {
        let annotations = tool.annotations.as_ref().unwrap();
        if tool.name == "resolute_reset" {
            assert_eq!(
                annotations.destructive_hint,
                Some(true),
                "resolute_reset should be marked destructive"
            );
        } else {
            assert_ne!(
                annotations.destructive_hint,
                Some(true),
                "tool '{}' should NOT be marked destructive",
                tool.name
            );
        }
    }
}

#[test]
fn read_only_tools_are_marked() {
    let svc = service();
    let read_only = ["resolute_status", "resolute_export"];
    for tool in svc.list_tools_view() {
        let annotations = tool.annotations.as_ref().unwrap();
        if read_only.contains(&tool.name.as_ref()) {
            assert_eq!(
                annotations.read_only_hint,
                Some(true),
                "tool '{}' should be read-only",
                tool.name
            );
        } else {
            assert_ne!(
                annotations.read_only_hint,
                Some(true),
                "tool '{}' should NOT be read-only",
                tool.name
            );
        }
    }
}

#[test]
fn every_tool_has_title() {
    let svc = service();
    for tool in svc.list_tools_view() {
        let annotations = tool.annotations.as_ref().unwrap();
        assert!(
            annotations.title.is_some(),
            "tool '{}' missing title annotation",
            tool.name
        );
    }
}

#[test]
fn tool_names_have_resolute_prefix() {
    let svc = service();
    for tool in svc.list_tools_view() {
        assert!(
            tool.name.starts_with("resolute_"),
            "tool '{}' should start with 'resolute_'",
            tool.name
        );
    }
}

#[test]
fn output_schema_is_valid_json_schema() {
    let svc = service();
    for tool in svc.list_tools_view() {
        let schema = tool.output_schema.as_ref().unwrap();
        assert!(
            schema.contains_key("type") || schema.contains_key("properties"),
            "tool '{}' output_schema doesn't look like a JSON Schema: {:?}",
            tool.name,
            schema.keys().collect::<Vec<_>>()
        );
    }
}
