//! The reasoning engine — `ReasoningServer` and everything it does.
//!
//! Split by concern from a 2884-line monolith in 0.3.0. Each submodule
//! owns one slice of `ReasoningServer`'s behavior:
//!
//! | Module     | Concern                                              |
//! |------------|------------------------------------------------------|
//! | `core`     | Struct, constructors, all engine methods (still big — incremental splits coming) |
//! | `recovery` | XML-injection diagnostics & repair                   |
//!
//! Multiple `impl ReasoningServer { ... }` blocks across files are how
//! Rust lets us decompose without splitting the type. The struct itself
//! stays single-source in `core`.

pub mod branching;
pub mod core;
pub mod export;
pub mod impact;
pub mod lookup;
pub mod mutations;
pub mod numbering;
pub mod process;
pub mod recovery;
pub mod revisions;
pub mod sessions;
pub mod snapshots;
pub mod validation;

pub use core::{ProcessErr, ProcessOk, ProcessResult, ReasoningServer};
