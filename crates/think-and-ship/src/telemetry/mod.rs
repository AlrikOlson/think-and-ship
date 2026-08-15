//! Structurally-anonymized telemetry (`telemetry-shape-extract`).
//!
//! The PURE half of the privacy-preserving telemetry program: local records
//! (as their canonical [`UnifiedRecordEnvelope`](crate::cloud::envelope)
//! projections) reduce to a [`shape::StructuralShape`] that carries graph
//! topology, tool sequences, status transitions, and duration buckets — and
//! provably **zero free text**. Every emitted string is either a member of a
//! closed vocabulary or a salted-hash pseudonym; [`scrub`] then re-verifies
//! the serialized output with a detector pass as defense-in-depth.
//!
//! No egress here. Consent (31h-b) and the wire (31h-c) build on top.

pub mod consent;
pub mod egress;
pub mod scrub;
pub mod shape;

pub use consent::{ConsentSource, ConsentState, should_send};
pub use egress::{TelemetryReport, build_report, load_or_create_salt, send_report};
pub use scrub::{ScrubFinding, scan};
pub use shape::{ShapeError, StructuralShape, extract};
