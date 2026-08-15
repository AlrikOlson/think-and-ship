//! An MCP server that gives a coding agent somewhere to write things down.
//!
//! MCP — the Model Context Protocol — is how an AI agent calls tools you
//! provide. This crate is one such tool provider. Point an MCP client at it and
//! the agent gains verbs for recording *why* it did something, *what* it
//! actually did, *what it plans to do next*, and *what people asked for* — as
//! structured records on disk instead of prose that scrolls out of the context
//! window.
//!
//! The problem it solves: an agent that reasons well in one session starts the
//! next one knowing nothing about it. Ask why a decision was made and the
//! honest answer is that nobody wrote it down. These records are the thing that
//! survives.
//!
//! # The tool families
//!
//! Every tool is prefixed with the family it belongs to, so a client's tool
//! list groups itself. There is deliberately no count here and no per-family
//! tool totals: a number nothing derives is a number that goes stale, which is
//! how this page came to claim two families when there were four.
//!
//! - `think_*` — the reasoning trace. Steps, branches, revisions, confidence,
//!   and conclusions worth pinning. Answers *why*.
//! - `ship_*` — the execution trace. Objectives, tasks, actions, quality gates,
//!   artifacts. Answers *what was actually done*.
//! - `roadmap_*` — the long-horizon plan the other two serve. Chunks of work
//!   with dependencies, statuses and priorities. It claims a second prefix,
//!   `tracker_*`, for mirroring that plan into an issue tracker through a
//!   provider-agnostic port — the same family, a different namespace.
//! - `signal_*` — what stakeholders raised: questions, ideas, concerns, bugs.
//!
//! They are not four separate logs. A reasoning step can point at the task it
//! motivated, a task can point back at the reasoning behind it, and both
//! resolve to the same project identity, so the graph reads end to end. The
//! links are typed — see [`infra::cross_ref::CrossRef`].
//!
//! # Getting started
//!
//! Install the binary and let it configure whichever MCP client you use:
//!
//! ```sh
//! npm install -g think-and-ship
//! cd your-project
//! think-and-ship init --full
//! ```
//!
//! Then, in a conversation, the agent records a step before it acts and an
//! outcome after — and the next session can read both back.
//!
//! # Using it as a library
//!
//! Most people want the binary. If you are embedding it instead, the composed
//! server is [`mcp::unified::UnifiedService`], and which families it exposes is
//! decided once at startup by [`mcp::unified::FamilySelection`]. Each family is
//! also usable on its own — [`think::ThinkService`], [`ship::ShipService`],
//! [`roadmap::RoadmapService`], [`signal::SignalService`] — and each wraps a
//! plain engine you can drive without any MCP wire at all, such as
//! [`roadmap::RoadmapEngine`] and [`signal::SignalEngine`].
//!
//! State is off by default and opt-in with `THINK_AND_SHIP_PERSIST=true`; see
//! [`infra::PersistenceConfig`].
//!
//! # Where to read next
//!
//! Design notes, the record schema and the wire contract live in the `docs/`
//! directory of the source repository, linked from this crate's listing.

pub mod cli;
pub mod cloud;
pub mod content;
pub mod corpus;
pub mod hygiene;
pub mod infra;
pub mod mcp;
pub mod otel;
pub mod otel_live;
pub mod otel_logs;
pub mod otel_receipt;
pub mod otlp_config;
pub mod roadmap;
pub mod ship;
pub mod signal;
pub mod telemetry;
pub mod think;
pub mod trace_context;
pub mod tracker;
pub mod usage;
