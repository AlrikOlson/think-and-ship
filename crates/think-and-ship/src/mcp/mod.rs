//! MCP wire adapter: the `ToolFamily` trait, family registry, output
//! schemas, tool annotations.

pub mod families;

pub use families::{AliasEntry, DispatchError, FamilyRegistry, ToolEntry, ToolFamily, ToolHandler, ToolResult};
