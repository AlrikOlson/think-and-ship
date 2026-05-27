//! MCP wire adapter: the `ToolFamily` trait, family registry, the
//! `UnifiedService` that exposes both families on one MCP server.

pub mod families;
pub mod unified;

pub use families::{AliasEntry, DispatchError, FamilyRegistry, ToolEntry, ToolFamily, ToolHandler, ToolResult};
pub use unified::{Family as UnifiedFamily, UnifiedService};
