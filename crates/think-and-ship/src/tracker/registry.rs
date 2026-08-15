//! The one place a tracker provider is registered.
//!
//! # What this closes
//!
//! The seam claimed "adding a provider touches no core file". That was precise
//! about the PORT and generous about the BINARY. Adding Linear really did cost
//! six inserted lines outside `linear.rs`, and four of them were in
//! `cli/mod.rs`: a match arm, plus the list of known providers typed a SECOND
//! time into the refusal message. The match arm is a cost; the second list is a
//! bug waiting for its first commit, because nothing made the two agree.
//!
//! After this module the cost is one line — an entry in [`PROVIDERS`] — and the
//! refusal reads its list from that same table, so the two cannot disagree.
//!
//! # Why an explicit table and not `linkme`/`inventory`
//!
//! A distributed slice would make the cost zero lines. It was rejected on its
//! FAILURE MODE, not on its dependency count. Slice elements defined in a
//! dependency crate can be discarded by the linker (`dtolnay/linkme#36`), the
//! mechanism carries platform-support limits that the Rust Internals
//! global-registration pre-RFC names as the reason it is not in the compiler,
//! and RUSTSEC-2024-0407 records the slice silently accepting an element of a
//! coerced type. Every one of those failures presents identically at runtime: a
//! provider is simply, silently absent. The explicit table's failure mode is "a
//! new adapter does not compile until you add its line", which a compiler
//! catches and a user never sees. That trade is worth one line per provider.
//!
//! # Why this is NOT the ingest registry, and cannot be
//!
//! This module was written expecting to be "the outbound mirror" of the
//! inbound ingest registry, sharing one definition of what a provider is.
//! They cannot share one, and the reason is not a scheduling accident:
//!
//! - `backend/src/contract.ts`'s `KnownAdapter` names an INGEST CHANNEL —
//!   `webhook`, `github_issue`, `email`, `submit_api`, `mcp`, `local`. It
//!   answers "how did this record arrive". The key here names a TRACKER —
//!   `github`, `linear`, `jira`. It answers "which system do we mirror into".
//!   `github_issue` and `github` are different values of different kinds, and
//!   collapsing them would make one of the two questions unanswerable.
//! - The ingest registry solved its half by deliberately STOPPING enumerating:
//!   `KnownAdapter | (string & {})` accepts adapters it has never heard of,
//!   because a closed union meant redeploying a Worker before one event could
//!   land, and constraining the SHAPE was enough. This half cannot copy that.
//!   An inbound adapter name only has to be recorded; an outbound provider key
//!   arrives from user config as an arbitrary string and must resolve to a
//!   CONSTRUCTOR. Something has to enumerate, or nothing can be built.
//!
//! What does carry over is the shape, which is the better half anyway:
//! `backend/src/ingest-registry.ts` has each provider's own file declare its own
//! const entry against one interface. That is exactly [`REGISTRATION`] here.
//!
//! [`REGISTRATION`]: crate::tracker::github::REGISTRATION

use crate::tracker::credential::Credential;
use crate::tracker::port::{TrackerError, TrackerPort};

/// Everything an adapter needs to be built, resolved ONCE by the caller.
///
/// Passed as a struct rather than as three arguments so that a future
/// construction input is added here, where every registered provider receives
/// it, instead of in each adapter's arm where a provider can be forgotten. That
/// is what already happened to trace context: it is applied by the builder
/// below rather than by remembering to call `with_trace_context` per arm.
#[derive(Debug, Clone, Copy)]
pub struct ProviderBuild<'a> {
    /// The opaque destination string the per-project config stores. Each
    /// adapter parses it in its own vocabulary — `owner/repo`, a team key — and
    /// refuses a shape it cannot use.
    pub target: &'a str,
    /// The project id whose caller trace context the adapter should adopt.
    pub project: &'a str,
    /// The resolved credential, if the human has connected one. `None` is a
    /// legitimate state: a read-only or unauthenticated call still builds.
    pub credential: Option<&'a Credential>,
}

/// One provider's entry in the registry, declared in that provider's own file.
///
/// `key` is the string a human types into `--provider` and the string that
/// appears in every link record. It is NOT independent of the adapter: the
/// port it builds answers the same question through
/// [`TrackerPort::provider`], and a registration whose two answers disagreed
/// would write link records under a provider that cannot read them back. The
/// truth gate for that lives in `tests/tracker_provider_registry.rs` and it
/// BUILDS each registration rather than comparing two lists.
pub struct ProviderRegistration {
    /// The provider key, lowercase. See [`crate::infra::cross_ref::ProviderId`].
    pub key: &'static str,
    /// Construct the adapter. A plain `fn` pointer rather than a boxed closure
    /// so the whole table is a `const` and cannot be built at runtime — there
    /// is no "register at startup" path to forget to call.
    pub build: fn(&ProviderBuild<'_>) -> Result<Box<dyn TrackerPort>, TrackerError>,
}

impl std::fmt::Debug for ProviderRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistration")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// THE registration point. Adding a tracker adds a file and this one line.
pub const PROVIDERS: &[ProviderRegistration] = &[
    crate::tracker::github::REGISTRATION,
    crate::tracker::linear::REGISTRATION,
    crate::tracker::projects_v2::REGISTRATION,
];

/// Why a provider could not be built.
///
/// Local to this module rather than a new [`TrackerError`] variant. That enum
/// documents why a tracker CALL failed and exists to answer
/// `TrackerError::retryable`; an unknown provider means no call was ever
/// attempted and no retry could ever help, so widening it there would make the
/// question the enum exists to answer harder to read.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The configured key matches no registration.
    #[error("'{provider}' is not a tracker this version can mirror into (known: {known})")]
    UnknownProvider {
        /// What the human actually typed.
        provider: String,
        /// The registered keys, rendered by [`known_list_in`]. DERIVED — this
        /// is the list that used to be typed a second time in `cli/mod.rs`.
        known: String,
    },
    /// The provider is registered, but refused the target or the credential.
    #[error(transparent)]
    Adapter(#[from] TrackerError),
}

/// Build the adapter registered under `provider`.
///
/// Unknown providers are NAMED rather than silently ignored — a typo in
/// `--provider` must not look like "nothing to do".
pub fn build(
    provider: &str,
    request: &ProviderBuild<'_>,
) -> Result<Box<dyn TrackerPort>, RegistryError> {
    build_in(PROVIDERS, provider, request)
}

/// The registered provider keys, in table order, as a human-readable list.
#[must_use]
pub fn known_list() -> String {
    known_list_in(PROVIDERS)
}

/// [`build`] against an arbitrary table.
///
/// The seam exists so that "adding a provider touches only its own file plus
/// the one registration point" is EXECUTABLE rather than asserted by grepping
/// this file: a test composes a third registration and drives the real lookup
/// and the real refusal through it, with no core file edited.
pub fn build_in(
    registrations: &[ProviderRegistration],
    provider: &str,
    request: &ProviderBuild<'_>,
) -> Result<Box<dyn TrackerPort>, RegistryError> {
    match registrations.iter().find(|r| r.key == provider) {
        Some(r) => Ok((r.build)(request)?),
        None => Err(RegistryError::UnknownProvider {
            provider: provider.to_string(),
            known: known_list_in(registrations),
        }),
    }
}

/// [`known_list`] against an arbitrary table. One renderer, so the refusal
/// message and anything else that advertises the set cannot word it differently.
#[must_use]
pub fn known_list_in(registrations: &[ProviderRegistration]) -> String {
    registrations
        .iter()
        .map(|r| r.key)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_known_list_is_rendered_from_the_table_it_is_given() {
        assert_eq!(known_list_in(&[]), "");
        assert_eq!(known_list_in(PROVIDERS), known_list());
        // Non-vacuity: the real table is not empty, so the equality above is a
        // claim about content rather than about two empty strings.
        assert!(known_list().contains("github"));
    }

    /// The module's front page must name every provider this build can reach.
    ///
    /// A doc paragraph listing adapters is a second copy of [`PROVIDERS`], and
    /// the second copy is the one that goes stale — the page said "GitHub,
    /// Linear and Jira" while the table held `github`, `linear` and
    /// `projects_v2`, so it named one destination that does not exist and
    /// omitted one that does. This binds the sentence to the table.
    ///
    /// It proves PRESENCE, not truth: a page reading "we do not support
    /// `linear`" would satisfy it. Presence is what a text gate can honestly
    /// check.
    #[test]
    fn the_module_front_page_names_every_registered_provider() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(concat!("src/tracker/", "mod.rs"));
        let source = crate::infra::source_gate::read_window(&path);

        // The leading run of `//!` lines: the front page, not the whole file.
        // Without this window, a key mentioned in one of the `pub mod`
        // declarations below would satisfy the assertion for free.
        let front_page: String = source
            .lines()
            .take_while(|l| l.trim_start().starts_with("//!") || l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !front_page.trim().is_empty(),
            "the tracker front page is empty — an empty page covers no provider"
        );

        for r in PROVIDERS {
            assert!(
                front_page.contains(r.key),
                "the tracker front page never names '{}', a provider this build \
                 registers and can mirror into:\n{front_page}",
                r.key
            );
        }

        // Non-vacuity: the search can fail. Without this a `contains` that
        // always returned true would pass every assertion above.
        assert!(
            !front_page.contains(concat!("nosuch", "provider")),
            "the front page names a provider that does not exist, so this gate's \
             search proves nothing"
        );
    }

    #[test]
    fn an_unknown_provider_is_named_and_so_is_every_registered_one() {
        let request = ProviderBuild {
            target: "owner/repo",
            project: "p",
            credential: None,
        };
        // `expect_err` needs `T: Debug`, and a port is a live client rather
        // than a value to print — so match, which also lets the failing branch
        // name the provider that wrongly resolved.
        let message = match build("githbu", &request) {
            Ok(port) => panic!("a typo resolved to '{}'", port.provider()),
            Err(error) => error.to_string(),
        };
        assert!(message.contains("githbu"), "the typo is named: {message}");
        for r in PROVIDERS {
            assert!(
                message.contains(r.key),
                "the refusal must name registered provider '{}': {message}",
                r.key
            );
        }
    }
}
