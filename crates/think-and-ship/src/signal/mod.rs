//! Signal domain and the `signal_*` tool family — the local surface of the
//! stakeholder-signal system. A signal is a question / idea /
//! concern / bug / feedback raised about the project; the local store is a
//! CACHE of the cloud system-of-record (the wire contract is
//! `contract/signal-envelope.schema.json`; the cloud client syncs against
//! it). Additive: a fourth family beside `think_*`, `ship_*`, and
//! `roadmap_*`, mirroring the roadmap module's domain → engine → mcp shape.

pub mod broadcast;
pub mod domain;
pub mod engine;
pub mod mcp;
pub mod output_schemas;

pub use engine::SignalEngine;
pub use mcp::SignalService;
