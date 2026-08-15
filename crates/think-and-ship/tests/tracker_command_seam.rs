//! The whole-command seam: a WHOLE tracker CLI
//! command, executed by a test, against a provider this crate has never heard
//! of and with no network anywhere.
//!
//! # What was actually wrong, which is not the obvious diagnosis
//!
//! The obvious diagnosis was a `build_tracker_port` that matched on the
//! provider string and knew only `github` and `linear`. By the time this file
//! was written that was false twice over: the provider list had moved into
//! [`PROVIDERS`], and `tracker::registry::build_in` already took the
//! registration table as a parameter — with
//! `tests/tracker_provider_registry.rs` already driving it through a table it
//! composed itself. The seam EXISTED and stopped one level too low. Every
//! tracker command reached its port through the arity that closes over
//! `PROVIDERS`, so a test could prove the registry dispatches to an invented
//! provider and still could not make a command do it.
//!
//! # Why the table, and not the two obvious alternatives
//!
//! The alternatives were a `cfg(test)` fake arm or a real,
//! production-reachable fake provider key. Both were rejected, and the argument is in
//! [`build_tracker_port_in`]'s doc comment rather than repeated here. The short
//! form: a `cfg(test)` arm means the tested path is not the shipped path, and a
//! production-reachable fake key is a way to point a human's mirror at nothing.
//! Passing the table costs neither, because there is exactly one
//! port-construction body and both callers run it.
//!
//! [`PROVIDERS`]: think_and_ship::tracker::PROVIDERS
//! [`build_tracker_port_in`]: think_and_ship::cli::build_tracker_port_in

use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::port::{TrackerError, TrackerPort};
use think_and_ship::tracker::registry::{PROVIDERS, ProviderBuild, ProviderRegistration};

/// The provider key this whole file turns on. Deliberately not a plausible
/// product name: if this string ever appears in [`PROVIDERS`], the test below
/// that says it must not is the one that will notice.
const FAKE_PROVIDER: &str = "seamco";

/// A destination the fake accepts and every real adapter would refuse. It is
/// never dialled — [`FakeTracker`] makes no network call — but it has to be
/// non-empty to clear the consent gate.
const FAKE_TARGET: &str = "seamco-workspace";

/// The chunk the command is asked to mirror.
const CHUNK_ID: &str = "a-chunk-the-command-must-actually-push";

/// A registration declared entirely in this test file. No core file knows it
/// exists, which is the property the end-to-end test is spending.
const SEAM_PROVIDER: ProviderRegistration = ProviderRegistration {
    key: FAKE_PROVIDER,
    build: build_seam_port,
};

fn build_seam_port(request: &ProviderBuild<'_>) -> Result<Box<dyn TrackerPort>, TrackerError> {
    // The adapter's own refusal, kept real rather than stubbed out: a
    // registration that accepted anything would make the "port construction
    // ran" half of this test unfalsifiable.
    if request.target.is_empty() {
        return Err(TrackerError::Unsupported(
            "the seam provider needs a target".into(),
        ));
    }
    Ok(Box::new(FakeTracker::new(FAKE_PROVIDER)))
}

/// The production table plus this file's registration.
///
/// Built rather than `const`, because [`ProviderRegistration`] holds a `&'static
/// str` and a fn pointer and cannot be concatenated at compile time. The
/// production entries are copied through verbatim so the command under test is
/// choosing from a SUPERSET of what ships, not from a replacement — a table
/// containing only the fake would prove dispatch works and prove nothing about
/// the real providers still being reachable beside it.
fn table_with_seam() -> Vec<ProviderRegistration> {
    let mut table: Vec<ProviderRegistration> = PROVIDERS
        .iter()
        .map(|r| ProviderRegistration {
            key: r.key,
            build: r.build,
        })
        .collect();
    table.push(SEAM_PROVIDER);
    table
}

const PROJECT_ID: &str = "tracker-command-seam";

/// The one scratch directory this binary's environment points at, for the whole
/// process and every test in it.
///
/// This is why the file is its own test binary, and why the redirection is a
/// `OnceLock` rather than a per-test call. `PersistenceConfig::from_env` and
/// `resolve_project_id` read process-global environment — the same hazard as
/// `tracing`'s global callsite cache in different clothes — so tests setting it
/// per-test would race. Worse, a test that DIDN'T set it would silently resolve
/// the developer's real data dir: `credential_resolver` reads stored credentials
/// from `PersistenceConfig::from_env().data_dir`, so every test here must be
/// inside the scratch before it builds any port, not only the one that writes.
fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    SCRATCH
        .get_or_init(|| {
            let dir = tempfile::TempDir::new().expect("tempdir");
            // SAFETY: set exactly once, before any test in this binary reads
            // them, and this binary runs nothing else.
            unsafe {
                std::env::set_var("THINK_AND_SHIP_DATA_DIR", dir.path());
                std::env::set_var("THINK_AND_SHIP_PERSIST", "true");
                std::env::set_var("THINK_AND_SHIP_PROJECT_NAME", PROJECT_ID);
            }
            dir
        })
        .path()
}

/// Write the tracker config the command will read back off disk, and the
/// opted-in chunk it is expected to mirror.
fn arrange() -> String {
    let data_dir = scratch();
    let project_id = PROJECT_ID;

    let config = think_and_ship::tracker::config::enable(
        data_dir,
        project_id,
        FAKE_PROVIDER,
        FAKE_TARGET,
        "2026-07-29T00:00:00+00:00",
    )
    .expect("the tracker config is written to the scratch data dir");
    assert!(
        think_and_ship::tracker::should_project(&config),
        "the arranged config must clear the consent gate the command checks first"
    );

    let mut engine = roadmap_engine(project_id);
    engine
        .add_chunk(
            CHUNK_ID.to_string(),
            "the chunk the seam pushes".to_string(),
            think_and_ship::roadmap::domain::ChunkStatus::Pending,
            100,
            "written by tracker_command_seam".to_string(),
            Vec::new(),
            Vec::new(),
            false,
        )
        .expect("the chunk is added");
    engine
        .set_tracker_opt_in(CHUNK_ID, FAKE_PROVIDER, true)
        .expect("the chunk opts in to the seam provider");

    project_id.to_string()
}

/// An engine over the same on-disk state the command loads, for arranging
/// before the run and for reading the verdict after it.
fn roadmap_engine(project_id: &str) -> think_and_ship::roadmap::RoadmapEngine {
    let persistence = think_and_ship::infra::Persistence::new(
        &think_and_ship::infra::PersistenceConfig::from_env(),
        think_and_ship::infra::Domain::Roadmap,
    );
    think_and_ship::roadmap::RoadmapEngine::new(project_id.to_string())
        .with_persistence(persistence)
}

/// THE TEST THIS FILE EXISTS FOR.
///
/// `tracker_push_in` is the whole command — config resolution, the consent
/// gate, port construction, the outbox, the projector and the link write-back.
/// None of it is an extracted core: the entry point a human's `tracker push`
/// reaches is one wrapper above this call, and that wrapper differs only in
/// passing [`PROVIDERS`].
///
/// The assertion is on what the command LEFT BEHIND ON DISK, not on the
/// double's bookkeeping. A tracker link for our chunk under our provider can
/// only exist if config resolution found the config, the consent gate passed,
/// the foreign table was consulted, the port it built accepted an upsert, and
/// the engine persisted the result. Reaching into the `FakeTracker` for a write
/// count would have proved a shorter chain.
#[test]
fn a_whole_tracker_command_runs_against_a_provider_this_crate_has_never_heard_of() {
    let project_id = arrange();

    think_and_ship::cli::tracker_push_in(&table_with_seam(), false)
        .expect("the push command completes against the seam provider");

    // LOAD-BEARING FIRST: the command wrote a link, which nothing but a
    // successful end-to-end run could have produced.
    let engine = roadmap_engine(&project_id);
    let link = engine
        .tracker_link(CHUNK_ID, FAKE_PROVIDER)
        .unwrap_or_else(|| {
            panic!("the command must have recorded a link for '{CHUNK_ID}' under {FAKE_PROVIDER}")
        });
    assert!(
        !link.external_id.is_empty(),
        "the link must carry the identity the port minted, not a placeholder"
    );
    assert_eq!(
        link.provider, FAKE_PROVIDER,
        "the link is filed under the provider the command was configured for"
    );
}

/// The command must refuse a provider that is in NO table, through the same
/// call the happy path uses.
///
/// Without this the test above is satisfied by a build that ignores its
/// argument entirely and returns something — deliberately breaking the lookup
/// showed exactly that. A dispatch that cannot refuse is not dispatching.
#[test]
fn the_same_command_refuses_a_provider_no_table_contains() {
    let _ = scratch();
    let table = table_with_seam();
    let config = think_and_ship::tracker::TrackerConfig {
        enabled: true,
        provider: Some("no-such-provider-anywhere".to_string()),
        target: Some(FAKE_TARGET.to_string()),
        ..think_and_ship::tracker::TrackerConfig::default()
    };

    let error = match think_and_ship::cli::tracker_port_in(&table, &config) {
        Ok(port) => panic!(
            "an unregistered provider must not build a port; got '{}'",
            port.provider()
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        error.contains("no-such-provider-anywhere"),
        "the refusal names what the human typed: {error}"
    );
    // …and it advertises the table it was actually given, fake included, which
    // is the proof that the refusal reads the same slice the lookup does.
    assert!(
        error.contains(FAKE_PROVIDER),
        "the refusal lists the table it was handed: {error}"
    );
}

/// The safety half, and the reason no `cfg(test)` arm was needed.
///
/// The fake is unreachable in production for the only durable reason: the
/// shipped table does not contain it. This asserts that directly, against the
/// real const rather than against a copy.
#[test]
fn no_production_provider_string_can_select_the_fake() {
    let _ = scratch();
    assert!(
        PROVIDERS.iter().all(|r| r.key != FAKE_PROVIDER),
        "the shipped table must never register '{FAKE_PROVIDER}'"
    );

    // The stronger form: the PRODUCTION arity refuses it. `tracker_port` is
    // private, so this goes through the same builder with the shipped table —
    // which is precisely what every real command passes.
    let config = think_and_ship::tracker::TrackerConfig {
        enabled: true,
        provider: Some(FAKE_PROVIDER.to_string()),
        target: Some(FAKE_TARGET.to_string()),
        ..think_and_ship::tracker::TrackerConfig::default()
    };
    match think_and_ship::cli::tracker_port_in(PROVIDERS, &config) {
        Ok(port) => panic!(
            "the shipped table built the test's fake provider as '{}'",
            port.provider()
        ),
        Err(e) => assert!(
            e.to_string().contains(FAKE_PROVIDER),
            "the shipped refusal names the provider it rejected: {e}"
        ),
    }
}
