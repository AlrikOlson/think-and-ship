//! Tool families: namespaced groups of MCP tools (`think_*`, `ship_*`, …)
//! plugged into one server via the [`ToolFamily`] trait.
//!
//! The wire adapter (which lives elsewhere in `mcp/`) iterates the
//! registered families to build the `list_tools` response and dispatches
//! incoming `call_tool` requests by tool name. This module is
//! transport-agnostic: payloads are `serde_json::Value` so the trait can
//! be unit-tested without an MCP transport.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

pub type ToolResult = Result<Value, DispatchError>;

pub type ToolHandler = Arc<dyn Fn(Value) -> ToolResult + Send + Sync>;

/// One tool exposed by a family.
#[derive(Clone)]
pub struct ToolEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub handler: ToolHandler,
}

impl std::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// A deprecated alias for an existing tool, served with a deprecation
/// warning during a migration window.
#[derive(Debug, Clone, Copy)]
pub struct AliasEntry {
    pub from: &'static str,
    pub to: &'static str,
    pub deprecation_warning: &'static str,
}

/// One family of tools (`think_*`, `ship_*`, …).
pub trait ToolFamily: Send + Sync {
    /// Namespace prefix (without trailing underscore): `"think"`, `"ship"`.
    fn prefix(&self) -> &'static str;

    /// Tools this family exposes, with handlers attached.
    fn tools(&self) -> Vec<ToolEntry>;

    /// Instructions text returned in the MCP `initialize` response.
    fn instructions(&self) -> &'static str;

    /// Optional: deprecated tool-name aliases (e.g.
    /// `"deliberate_record_step"` → `"think_record_step"`).
    fn deprecated_aliases(&self) -> Vec<AliasEntry> {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    UnknownTool(String),
    Handler(String),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "unknown tool '{name}'"),
            Self::Handler(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DispatchError {}

/// Holds registered families and a flat lookup table for dispatch.
pub struct FamilyRegistry {
    families: Vec<Box<dyn ToolFamily>>,
    tools: HashMap<String, ToolEntry>,
    aliases: HashMap<String, AliasEntry>,
}

impl FamilyRegistry {
    pub fn new() -> Self {
        Self {
            families: Vec::new(),
            tools: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Register a family. Duplicate tool or alias names panic — this is a
    /// startup configuration error, not a runtime condition to recover from.
    pub fn register(&mut self, family: Box<dyn ToolFamily>) {
        for tool in family.tools() {
            if self.tools.insert(tool.name.to_string(), tool).is_some() {
                panic!("duplicate tool name registered in FamilyRegistry");
            }
        }
        for alias in family.deprecated_aliases() {
            if self.aliases.insert(alias.from.to_string(), alias).is_some() {
                panic!("duplicate alias name registered in FamilyRegistry");
            }
        }
        self.families.push(family);
    }

    /// All registered tools across all families. Caller is responsible for
    /// any sort order; iteration order is insertion order of the families
    /// followed by intra-family order.
    pub fn all_tools(&self) -> Vec<&ToolEntry> {
        self.families
            .iter()
            .flat_map(|f| f.tools().into_iter().map(|t| t.name))
            .filter_map(|name| self.tools.get(name))
            .collect()
    }

    /// All registered alias entries (so the wire adapter can attach the
    /// `_meta.deprecation_warning` annotation per the MCP spec).
    pub fn all_aliases(&self) -> Vec<&AliasEntry> {
        self.families
            .iter()
            .flat_map(|f| f.deprecated_aliases().into_iter().map(|a| a.from))
            .filter_map(|name| self.aliases.get(name))
            .collect()
    }

    /// Dispatch `tool_name(args)` to the matching handler. Resolves
    /// deprecated aliases transparently.
    pub fn dispatch(&self, tool_name: &str, args: Value) -> ToolResult {
        if let Some(alias) = self.aliases.get(tool_name) {
            return self.dispatch_canonical(alias.to, args);
        }
        self.dispatch_canonical(tool_name, args)
    }

    fn dispatch_canonical(&self, name: &str, args: Value) -> ToolResult {
        self.tools
            .get(name)
            .ok_or_else(|| DispatchError::UnknownTool(name.to_string()))
            .and_then(|t| (t.handler)(args))
    }
}

impl Default for FamilyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoFamily;

    impl ToolFamily for EchoFamily {
        fn prefix(&self) -> &'static str {
            "echo"
        }
        fn tools(&self) -> Vec<ToolEntry> {
            vec![
                ToolEntry {
                    name: "echo_repeat",
                    description: "Echo the input back.",
                    handler: Arc::new(Ok),
                },
                ToolEntry {
                    name: "echo_count",
                    description: "Count keys.",
                    handler: Arc::new(|v| {
                        let n = v.as_object().map(|o| o.len()).unwrap_or(0);
                        Ok(serde_json::json!({ "count": n }))
                    }),
                },
            ]
        }
        fn instructions(&self) -> &'static str {
            "echo family for tests"
        }
        fn deprecated_aliases(&self) -> Vec<AliasEntry> {
            vec![AliasEntry {
                from: "old_repeat",
                to: "echo_repeat",
                deprecation_warning: "renamed to echo_repeat",
            }]
        }
    }

    #[test]
    fn registers_and_dispatches_a_tool() {
        let mut reg = FamilyRegistry::new();
        reg.register(Box::new(EchoFamily));
        let out = reg.dispatch("echo_repeat", serde_json::json!({"k": 1})).unwrap();
        assert_eq!(out, serde_json::json!({"k": 1}));
    }

    #[test]
    fn dispatch_resolves_aliases() {
        let mut reg = FamilyRegistry::new();
        reg.register(Box::new(EchoFamily));
        let out = reg.dispatch("old_repeat", serde_json::json!({"hello": "world"})).unwrap();
        assert_eq!(out, serde_json::json!({"hello": "world"}));
    }

    #[test]
    fn unknown_tool_yields_dispatch_error() {
        let reg = FamilyRegistry::new();
        let err = reg.dispatch("nope", Value::Null).unwrap_err();
        assert_eq!(err, DispatchError::UnknownTool("nope".to_string()));
    }

    #[test]
    fn handler_is_invoked_with_args() {
        let mut reg = FamilyRegistry::new();
        reg.register(Box::new(EchoFamily));
        let out = reg
            .dispatch("echo_count", serde_json::json!({"a": 1, "b": 2, "c": 3}))
            .unwrap();
        assert_eq!(out["count"], 3);
    }

    #[test]
    fn all_tools_returns_registered_entries() {
        let mut reg = FamilyRegistry::new();
        reg.register(Box::new(EchoFamily));
        let tools = reg.all_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert!(names.contains(&"echo_repeat"));
        assert!(names.contains(&"echo_count"));
    }

    #[test]
    fn all_aliases_returns_registered_aliases() {
        let mut reg = FamilyRegistry::new();
        reg.register(Box::new(EchoFamily));
        let aliases = reg.all_aliases();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].from, "old_repeat");
        assert_eq!(aliases[0].to, "echo_repeat");
    }
}
