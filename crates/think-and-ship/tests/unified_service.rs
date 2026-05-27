//! End-to-end smoke test for the UnifiedService: both families register
//! together and expose the full 44-tool surface (11 canonical + 11 alias
//! per family).

use std::collections::BTreeSet;

use think_and_ship::mcp::UnifiedService;
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::DeliberateConfig;
use think_and_ship::think::engine::core::ReasoningServer;

fn build_unified() -> UnifiedService {
    let mut cfg = DeliberateConfig::default();
    cfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(cfg));
    let ship = ShipService::new(ShipEngine::new("test-abc123".into()));
    UnifiedService::new(think, ship)
}

#[test]
fn lists_44_tools_with_both_canonicals_and_aliases() {
    let svc = build_unified();
    let tools = svc.list_tools_view();
    assert_eq!(
        tools.len(),
        44,
        "expected 11 think_* + 11 deliberate_* + 11 ship_* + 11 resolute_*, got {}",
        tools.len()
    );

    let names: BTreeSet<String> = tools.iter().map(|t| t.name.to_string()).collect();
    let think_count = names.iter().filter(|n| n.starts_with("think_")).count();
    let deliberate_count = names.iter().filter(|n| n.starts_with("deliberate_")).count();
    let ship_count = names.iter().filter(|n| n.starts_with("ship_")).count();
    let resolute_count = names.iter().filter(|n| n.starts_with("resolute_")).count();
    assert_eq!(think_count, 11, "11 think_* tools");
    assert_eq!(deliberate_count, 11, "11 deliberate_* aliases");
    assert_eq!(ship_count, 11, "11 ship_* tools");
    assert_eq!(resolute_count, 11, "11 resolute_* aliases");
}

#[test]
fn aliases_carry_deprecation_warning() {
    let svc = build_unified();
    for tool in svc.list_tools_view() {
        let is_alias = tool.name.starts_with("deliberate_") || tool.name.starts_with("resolute_");
        if !is_alias {
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
            "alias {} should mention deprecation, got {warning:?}",
            tool.name
        );
    }
}

#[test]
fn route_of_classifies_tools_by_family() {
    use think_and_ship::mcp::UnifiedFamily;
    assert_eq!(UnifiedService::route_of("think_record_step"), Some(UnifiedFamily::Think));
    assert_eq!(UnifiedService::route_of("deliberate_record_step"), Some(UnifiedFamily::Think));
    assert_eq!(UnifiedService::route_of("ship_set_objective"), Some(UnifiedFamily::Ship));
    assert_eq!(UnifiedService::route_of("resolute_set_objective"), Some(UnifiedFamily::Ship));
    assert!(UnifiedService::route_of("audit_anything").is_none());
}
