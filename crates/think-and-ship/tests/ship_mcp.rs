use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::ship::mcp::ShipService;

fn service() -> ShipService {
    let engine = ShipEngine::new("test-abc123".into());
    ShipService::new(engine)
}

#[test]
fn tools_list_has_22_entries() {
    let svc = service();
    let tools = svc.list_tools_view();
    assert_eq!(
        tools.len(),
        22,
        "expected 11 ship_* + 11 resolute_* aliases, got {}",
        tools.len()
    );
    let ship_count = tools.iter().filter(|t| t.name.starts_with("ship_")).count();
    let alias_count = tools
        .iter()
        .filter(|t| t.name.starts_with("resolute_"))
        .count();
    assert_eq!(ship_count, 11);
    assert_eq!(alias_count, 11);
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
    let destructive_names = ["ship_reset", "resolute_reset"];
    for tool in svc.list_tools_view() {
        let annotations = tool.annotations.as_ref().unwrap();
        if destructive_names.contains(&tool.name.as_ref()) {
            assert_eq!(
                annotations.destructive_hint,
                Some(true),
                "tool '{}' should be marked destructive",
                tool.name
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
    let read_only = [
        "ship_status",
        "ship_export",
        "resolute_status",
        "resolute_export",
    ];
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
fn tool_names_split_into_ship_canonicals_and_resolute_aliases() {
    let svc = service();
    for tool in svc.list_tools_view() {
        assert!(
            tool.name.starts_with("ship_") || tool.name.starts_with("resolute_"),
            "tool '{}' should start with 'ship_' or 'resolute_'",
            tool.name
        );
    }
}

#[test]
fn resolute_aliases_carry_deprecation_warning() {
    let svc = service();
    for tool in svc.list_tools_view() {
        if !tool.name.starts_with("resolute_") {
            continue;
        }
        let meta = tool
            .meta
            .as_ref()
            .unwrap_or_else(|| panic!("alias {} missing _meta", tool.name));
        let warning = meta
            .0
            .get("deprecation_warning")
            .unwrap_or_else(|| panic!("alias {} missing deprecation_warning", tool.name));
        assert!(
            warning.as_str().is_some_and(|s| s.contains("deprecated")),
            "alias {} should carry a deprecation message",
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
