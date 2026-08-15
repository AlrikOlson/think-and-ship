//! Roadmap domain and the `roadmap_*` tool family — the long-horizon
//! plan-of-plans that sits above `ship_*` objectives and links to `think_*`
//! reasoning. The foundation is the domain + engine + persistence; the rmcp
//! wire adapter (`mcp::RoadmapService`) puts `roadmap_*` on the live MCP wire.

pub mod broadcast;
pub mod domain;
pub mod engine;
pub mod import;
pub mod mcp;
pub mod name;
pub mod output_schemas;
pub mod region;

pub use engine::RoadmapEngine;
pub use mcp::RoadmapService;
