//! MCP wire-adapter surface.
//!
//! This module owns everything the MCP client touches:
//!
//! * [`service::ThinkService`] — the `ServerHandler` impl, including
//!   the custom `list_tools` that patches `output_schema` onto each entry,
//! * the 11 `#[tool]` handler methods in [`handlers`],
//! * input-argument types in [`args`],
//! * description strings in [`descriptions`],
//! * `instructions` text in [`instructions`].
//!
//! The wider crate never imports from these submodules directly. Only the
//! `ThinkService` re-exported here, plus the legacy [`crate::think::domain`]
//! re-exports of arg structs, are part of the public API.

pub mod args;
pub mod handlers;
pub mod instructions;
pub mod service;

pub use service::ThinkService;
