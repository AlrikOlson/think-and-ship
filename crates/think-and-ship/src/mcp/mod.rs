//! MCP wire adapter: the `UnifiedService` that exposes every family on one
//! MCP server.

pub mod cache;
pub mod client_view;
pub mod elicit;
pub mod progress;
pub(crate) mod resources;
pub mod schema_sanitize;
pub mod tasks;
pub mod unified;

pub use client_view::ClientView;
pub use progress::Heartbeat;
pub use unified::{Family as UnifiedFamily, UnifiedService};
