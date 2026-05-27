//! Pure domain types — no logic, no IO, no MCP wire concerns.
//!
//! These types are the contract between the engine, persistence, broadcast,
//! and the Tauri viewer. They're deliberately small and trait-free so the
//! viewer can deserialize the same bytes that the engine produced.

pub mod branch;
pub mod dep_edge;
pub mod history;
pub mod session;
pub mod step;

pub use branch::{Branch, BranchStatus};
pub use dep_edge::DepEdge;
pub use history::{DeliberateHistory, HistoryMetadata};
pub use session::SessionEntry;
pub use step::{DeliberateStep, NextAction, StructuredAction};
