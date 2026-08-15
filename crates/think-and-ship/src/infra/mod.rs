//! Shared infrastructure used by both tool families: project identity,
//! sessions, persistence, broadcast, typed cross-references.

pub mod broadcast;
pub mod coerce;
pub mod cross_ref;
pub mod persistence;
pub mod project_id;
pub mod repo_sync;
pub mod session;
#[cfg(test)]
pub mod source_gate;
pub mod tool_result;

pub use broadcast::{Broadcaster, EmitError, Family};
pub use cross_ref::{
    ActionId, CheckName, CrossRef, ExternalId, ParseError, ProviderId, StepNumber, TaskId,
};
pub use persistence::{Domain, Persistence, PersistenceConfig};
pub use project_id::{
    IdSource, PROJECT_DIR, PROJECT_FILE, PROJECT_SEP, ProjectIdentity, declared_identity_in,
    find_project_file, namespace_session_id, project_display_name, project_id_for_path,
    resolve_project_id, resolve_project_id_with, write_project_file,
};
pub use repo_sync::{
    MirrorJob, MirrorWorker, PromoteOutcome, RecordCtx, RepoSink, SyncTarget, discover_repo_root,
    file_attribution, shared_from_env,
};
pub use session::resolve_default_session_id;
