//! The registry's truth gates.
//!
//! `tracker-provider-registry` exists because "adding a provider adds a file"
//! was true of the port and not of the binary: the CLI matched on two string
//! literals and typed the list of known providers a SECOND time into its
//! refusal, with nothing making the two agree. These tests hold the repaired
//! shape, and each one is aimed at a different way it could rot.

use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::port::{TrackerError, TrackerPort};
use think_and_ship::tracker::registry::{
    self, PROVIDERS, ProviderBuild, ProviderRegistration, RegistryError,
};

/// Targets this test knows how to spell. Each adapter parses the destination in
/// its own vocabulary — GitHub wants `owner/repo`, Linear wants a bare team key
/// — so a provider-agnostic test cannot hold ONE sample. It holds a few and
/// takes the first that builds, and says exactly what to do when none does.
const SAMPLE_TARGETS: &[&str] = &["owner/repo", "ENG", "orgs/acme/projects/12"];

fn build_with_any_sample(key: &str) -> Box<dyn TrackerPort> {
    for target in SAMPLE_TARGETS {
        let request = ProviderBuild {
            target,
            project: "registry-test",
            credential: None,
        };
        if let Ok(port) = registry::build(key, &request) {
            return port;
        }
    }
    panic!(
        "no sample target in SAMPLE_TARGETS builds provider '{key}'; \
         add one this adapter accepts"
    )
}

/// `expect_err` is unavailable here — `Box<dyn TrackerPort>` is not `Debug`,
/// deliberately, since a port is a live client and not a value to print. The
/// unwrap this replaces would also have said less: on the failing branch this
/// names the provider that wrongly resolved.
fn refusal(result: Result<Box<dyn TrackerPort>, RegistryError>) -> RegistryError {
    match result {
        Ok(port) => panic!(
            "expected a refusal; got a port calling itself '{}'",
            port.provider()
        ),
        Err(error) => error,
    }
}

/// THE gate this file is really about, and it is not about the message.
///
/// A registration's `key` is what a human types into `--provider` and what every
/// link record is filed under. The port it builds answers the same question
/// through [`TrackerPort::provider`]. If those two ever disagreed, the projector
/// would write link records under a provider that cannot read them back — a
/// silent corruption, not a failed run.
///
/// Each adapter file now binds both to one `const PROVIDER`, so the drift is
/// unrepresentable WITHIN a file. What remains representable is a `REGISTRATION`
/// block copy-pasted into a sibling adapter and only half-renamed, which is
/// exactly why this test BUILDS every registration instead of comparing two
/// lists.
#[test]
fn every_registration_key_is_the_provider_the_port_it_builds_reports() {
    assert!(
        PROVIDERS.len() >= 2,
        "non-vacuity: the table must hold real providers, not zero"
    );

    let mut seen: Vec<&str> = Vec::new();
    for registration in PROVIDERS {
        let port = build_with_any_sample(registration.key);
        assert_eq!(
            registration.key,
            port.provider(),
            "registration '{}' builds a port that calls itself '{}'",
            registration.key,
            port.provider()
        );
        assert!(
            !seen.contains(&registration.key),
            "'{}' is registered twice — the later entry is unreachable",
            registration.key
        );
        seen.push(registration.key);
    }

    // Non-vacuity of the loop itself: two DISTINCT providers really ran through
    // it, so a table of two identical entries could not have passed silently.
    assert!(
        seen.contains(&"github") && seen.contains(&"linear"),
        "{seen:?}"
    );
}

/// The message half. The list used to be typed a second time in `cli/mod.rs`;
/// now it is rendered from the same table the lookup walks, so adding a
/// provider updates the refusal without anyone remembering to.
#[test]
fn the_refusal_reads_its_known_list_from_the_table_rather_than_a_second_copy() {
    let request = ProviderBuild {
        target: "owner/repo",
        project: "registry-test",
        credential: None,
    };

    let error = refusal(registry::build("githbu", &request));
    let message = error.to_string();

    // Load-bearing FIRST: the advertised list is character-for-character the
    // one the registry renders, so no second copy can drift from it.
    assert!(
        message.contains(&registry::known_list()),
        "the refusal must carry the rendered list verbatim: {message}"
    );
    for registration in PROVIDERS {
        assert!(
            message.contains(registration.key),
            "the refusal must name '{}': {message}",
            registration.key
        );
    }
    // A typo must be NAMED — an unknown provider that read as "nothing to do"
    // is the failure this message exists to prevent.
    assert!(message.contains("githbu"), "{message}");
    assert!(matches!(error, RegistryError::UnknownProvider { .. }));

    // Non-vacuity: the same request DOES resolve for a registered provider, so
    // the refusal above is about the key and not about the target.
    assert!(registry::build("github", &request).is_ok());
}

/// A provider that is registered but cannot use the target is a DIFFERENT
/// refusal from one that is not registered at all, and the difference has to
/// survive the registry — otherwise a malformed `--into` reads as "we do not
/// support GitHub".
#[test]
fn a_registered_provider_that_refuses_its_target_is_not_reported_as_unknown() {
    let request = ProviderBuild {
        target: "not-a-repo",
        project: "registry-test",
        credential: None,
    };

    let error = refusal(registry::build("github", &request));
    assert!(
        matches!(error, RegistryError::Adapter(TrackerError::Unsupported(_))),
        "expected the adapter's own refusal, got {error:?}"
    );
    // And it says what a repository looks like, rather than listing providers.
    let message = error.to_string();
    assert!(message.contains("owner/repo"), "{message}");
    assert!(
        !message.contains(&registry::known_list()),
        "a target problem must not be dressed up as an unknown provider: {message}"
    );
}

/// A provider declared entirely outside the crate's own adapter set, to prove
/// the registry's central claim: adding a provider
/// touches its own file plus the ONE registration point, and nothing else.
///
/// This registration lives in a test file. No core file knows it exists.
const THIRD_PARTY: ProviderRegistration = ProviderRegistration {
    key: "acme",
    build: build_acme,
};

fn build_acme(request: &ProviderBuild<'_>) -> Result<Box<dyn TrackerPort>, TrackerError> {
    if request.target.is_empty() {
        return Err(TrackerError::Unsupported("acme needs a target".into()));
    }
    Ok(Box::new(FakeTracker::new("acme")))
}

/// The executable form of "one registration point". If the lookup or the
/// message had a second, hardcoded notion of which providers exist, a table
/// this test composes could not be reached through them.
#[test]
fn a_provider_this_crate_has_never_heard_of_is_reachable_through_the_one_lookup() {
    let mut table: Vec<ProviderRegistration> = Vec::new();
    for registration in PROVIDERS {
        table.push(ProviderRegistration {
            key: registration.key,
            build: registration.build,
        });
    }
    table.push(THIRD_PARTY);

    let request = ProviderBuild {
        target: "anything",
        project: "registry-test",
        credential: None,
    };

    // Load-bearing FIRST: the third provider really builds, through the same
    // verb the CLI calls.
    let port = registry::build_in(&table, "acme", &request).expect("the third provider builds");
    assert_eq!(port.provider(), "acme");

    // …and it is advertised by the same renderer, without a core edit.
    let known = registry::known_list_in(&table);
    assert!(known.ends_with("acme"), "{known}");
    let error = refusal(registry::build_in(&table, "nope", &request));
    assert!(error.to_string().contains("acme"), "{error}");

    // The proof that no core file was edited: the real table still does not
    // know this provider, and the real lookup still refuses it.
    assert!(PROVIDERS.iter().all(|r| r.key != "acme"));
    assert!(registry::build("acme", &request).is_err());
}

/// The binary's half of the debt. `cli/mod.rs` used to name both providers and
/// hold the second copy of the list; the point of the registry is that it now
/// names none.
///
/// # This gate's excuse is retired, and it is kept anyway
///
/// It used to say: "a source-text gate, because the property IS about the text:
/// the function is not reachable from a test."
/// The second half of that is no longer true. The command seam shipped,
/// the builder takes its registration table as a parameter, and
/// `tests/tracker_command_seam.rs` now drives a WHOLE `tracker push` through it
/// against a provider this crate has never heard of.
///
/// So why is this still here? Because the two prove different things, and the
/// difference has been paid for before by deliberately breaking the source to
/// check what each gate notices. The executing test proves the dispatch CONSULTS THE TABLE IT WAS
/// GIVEN. It would pass just as happily on a builder that also carried a
/// hardcoded `if provider == "github"` shortcut ahead of the lookup, because
/// the seam test never asks for `github`. Only reading the text can show that
/// no such branch exists. Presence is behavioural; ABSENCE is textual. Neither
/// gate subsumes the other, and this one no longer needs an excuse to say so.
#[test]
fn the_cli_port_builder_names_no_provider() {
    let source = include_str!("../src/cli/mod.rs");
    // Needle assembled at runtime so this file's own source cannot satisfy the
    // search it performs.
    let signature = concat!("fn ", "build_tracker_port_in(");
    let start = source.find(signature).expect(
        "build_tracker_port_in still exists — a gate that cannot find its window covers nothing",
    );
    let body = &source[start..];
    let end = body.find("\n}\n").expect("the function terminates");
    let body = &body[..end];

    // Positive FIRST, because a source-text gate that only asserts an ABSENCE
    // passes just as happily on a file that no longer builds a port at all.
    //
    // Matched on a LIVE line rather than on the substring anywhere, so a
    // commented-out or otherwise dead call cannot satisfy it. Deliberately not
    // matched on the exact statement: this gate should bite on "the delegation
    // is gone", not on "someone bound the result to a local first".
    //
    // `build_in`, not `build`: the whole point of the command seam is that
    // this body dispatches into the table it was HANDED. A delegation to the
    // arity that closes over `PROVIDERS` would restore the blockage while
    // looking, to a careless reader, like the same line.
    let delegation = concat!("crate::tracker::registry::", "build_in(");
    assert!(
        body.lines().any(|line| {
            let code = line.trim_start();
            !code.starts_with("//") && code.contains(delegation)
        }),
        "build_tracker_port_in must delegate to the registry's table-taking lookup on a live line:\n{body}"
    );
    for registration in PROVIDERS {
        let literal = format!("\"{}\"", registration.key);
        assert!(
            !body.contains(&literal),
            "build_tracker_port_in names '{}' — the registry exists so it does not:\n{body}",
            registration.key
        );
    }
}

/// The board's registration criterion, as a behaviour rather than a claim: the board is
/// reachable through the ONE lookup, and it is a DIFFERENT destination from the
/// repository's issues rather than an alias for them.
///
/// The second half is the one worth holding. A board and a repo are both
/// "GitHub", and collapsing them onto one key would look tidy right up until
/// two link records for the same chunk fought over one `external_id`. The proof
/// that they have not been collapsed is that each refuses the other's target.
#[test]
fn the_board_is_reachable_through_the_one_lookup_and_is_not_the_issues_provider() {
    let board_target = ProviderBuild {
        target: "orgs/acme/projects/12",
        project: "registry-test",
        credential: None,
    };

    let board = registry::build("github_projects", &board_target)
        .unwrap_or_else(|e| panic!("the board must be reachable through the registry: {e}"));
    assert_eq!(
        board.provider(),
        "github_projects",
        "the key it was asked for is the key it files links under"
    );
    assert_ne!(
        board.provider(),
        "github",
        "a board and a repository's issues are two destinations, not one"
    );

    // Neither adapter accepts the other's address, which is what makes the two
    // keys a real distinction instead of a naming convention.
    match registry::build("github", &board_target) {
        Ok(port) => panic!(
            "a board address must not build the issues adapter; got '{}'",
            port.provider()
        ),
        Err(RegistryError::Adapter(TrackerError::Unsupported(_))) => {}
        Err(e) => panic!("expected a target refusal from the issues adapter, got {e:?}"),
    }
    let repo_target = ProviderBuild {
        target: "owner/repo",
        project: "registry-test",
        credential: None,
    };
    match registry::build("github_projects", &repo_target) {
        Ok(port) => panic!(
            "a repository address must not build the board adapter; got '{}'",
            port.provider()
        ),
        Err(RegistryError::Adapter(TrackerError::Unsupported(_))) => {}
        Err(e) => panic!("expected a target refusal from the board adapter, got {e:?}"),
    }
}
