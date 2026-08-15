//! Cloud sync. The local think-and-ship store is a CACHE of the
//! cloud system-of-record (the per-tenant backend). This module builds the
//! wire records it syncs — the unified record envelope contract.
//!
//! The pure, network-free builder lives here; the `SyncTarget::Cloud` HTTP
//! client (push/reconcile/offline) lives in the sibling modules.

pub mod backfill;
pub mod build;
pub mod client;
pub mod config;
pub mod connection;
pub mod credential;
pub mod device_flow;
pub mod envelope;
pub mod events;
pub mod outbox;
pub mod pull;

/// STRUCTURAL gates over the cloud sync test family — see
/// [`crate::infra::source_gate`] for why a structural gate exists at all and
/// what one is honestly worth.
#[cfg(test)]
mod sync_test_gates {
    use crate::infra::source_gate::{count_live, fn_blocks, read_window};

    /// The four integration tests that prove a local mutation reaches the
    /// cloud. Every one wires an engine with `.with_cloud(..)` and then asserts
    /// the backend saw it.
    fn family() -> Vec<std::path::PathBuf> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        ["roadmap", "ship", "signal", "think"]
            .iter()
            .map(|f| manifest.join(format!("tests/cloud_{f}_sync.rs")))
            .collect()
    }

    /// REACHABILITY, not behaviour: every cloud sync test that wires an engine
    /// to a backend must wait on the request ARRIVING, not on a duration.
    ///
    /// This gate exists because deleting the wait at a CALL SITE is very nearly
    /// invisible to the suite. The push is fire-and-forget on a detached task,
    /// so its absence does not produce a failure — it produces a *probability*
    /// of one. Measured with the wait removed: 18 failures in 120 runs, so
    /// deleting it still passes five times out of six. Every one of the
    /// four sites previously waited a fixed 400ms against a push that actually
    /// lands in 2.4ms (p50) — a constant nobody measured, unfalsifiable in both
    /// directions, and costing 1.6s of pure sleeping per `cargo test`.
    ///
    /// The window is asserted as hard as the rule: an unreadable path panics
    /// rather than silently covering nothing, and the site count is pinned so a
    /// renamed file — or a new test nobody noticed — fails loudly instead of
    /// open. Blocks are split per function so a wait in a NEIGHBOURING test
    /// cannot vouch for the call site in this one.
    ///
    /// This is a text gate, not a semantic one. It proves a wait is present,
    /// not that it waits for the right thing.
    #[test]
    fn every_cloud_sync_test_waits_for_the_request_rather_than_a_duration() {
        // Split so this gate's own source cannot satisfy its own needles.
        let wires_cloud = concat!(".with", "_cloud(");
        // Both named condition helpers from tests/support: the wiremock twins
        // wait on a recorded request, the live test waits on the cloud serving
        // the record back. Nothing else counts — in particular a bare deadline
        // loop written inline does not, because the next one would be written
        // slightly differently and this gate would stop seeing it.
        let waiters = [concat!("wait_for", "_request("), concat!("wait", "_until(")];
        let mut total_sites = 0usize;

        for path in family() {
            let src = read_window(&path);
            for (fname, block) in fn_blocks(&src) {
                if count_live(&block, wires_cloud) == 0 {
                    continue;
                }
                total_sites += 1;
                let waits: usize = waiters.iter().map(|w| count_live(&block, w)).sum();
                assert!(
                    waits >= 1,
                    "{}::{fname} wires an engine to a cloud backend but never waits for \
                     the request to arrive. The push is fire-and-forget on a detached \
                     task, so an assertion placed after it is racing that task. Use \
                     support::wait_for_request — a sleep is a race that merely usually \
                     wins, and its absence costs only ~15% of runs, which no assertion \
                     here can see.",
                    path.display(),
                );
            }
        }

        assert_eq!(
            total_sites, 5,
            "expected 5 cloud-wiring test sites across the cloud sync family; found \
             {total_sites}. If a file moved or was renamed this gate stopped covering \
             it — fix the window, do not adjust the count to match.",
        );
    }

    /// The anti-regression half: no test in this family may synchronise with a
    /// bare sleep again.
    ///
    /// Reachability alone would be satisfied by a test that waits properly AND
    /// keeps a leftover sleep, which is how the 400ms would creep back — as
    /// belt-and-braces, then as the only thing holding it up. A poll interval
    /// inside a bounded condition is a different animal and lives in
    /// `tests/support/mod.rs`, outside this window, so it is not caught here.
    ///
    /// The `#[ignore]`d live test is deliberately in scope: it faces a real
    /// network, whose tail is unbounded, which is the one place a guessed
    /// constant is guaranteed to be wrong eventually.
    #[test]
    fn no_cloud_sync_test_synchronises_with_a_bare_sleep() {
        // Split so this gate's own source cannot satisfy its own needles.
        let tokio_sleep = concat!("tokio::time", "::sleep(");
        let thread_sleep = concat!("thread", "::sleep(");

        for path in family() {
            let src = read_window(&path);
            for (fname, block) in fn_blocks(&src) {
                let sleeps = count_live(&block, tokio_sleep) + count_live(&block, thread_sleep);
                assert_eq!(
                    sleeps,
                    0,
                    "{}::{fname} sleeps {sleeps} time(s) to synchronise with a \
                     fire-and-forget push. The measured push lands in 2.4ms (p50) and \
                     44ms (worst contended); any constant chosen against that is a \
                     guess that fails silently when it is too small and costs wall \
                     clock when it is too large. Wait on the condition instead.",
                    path.display(),
                );
            }
        }
    }
}
