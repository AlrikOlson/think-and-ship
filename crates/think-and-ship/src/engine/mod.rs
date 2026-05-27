//! Shared infrastructure used by both tool families: project identity,
//! sessions, persistence, broadcast, typed cross-references.

pub mod broadcast;
pub mod cross_ref;
pub mod persistence;
pub mod project_id;
pub mod session;

pub use broadcast::{Broadcaster, EmitError, Family};
pub use cross_ref::{ActionId, CheckName, CrossRef, ParseError, StepNumber, TaskId};
pub use persistence::{Domain, Persistence, PersistenceConfig};
pub use project_id::{PROJECT_SEP, namespace_session_id, resolve_project_id};
pub use session::resolve_default_session_id;
