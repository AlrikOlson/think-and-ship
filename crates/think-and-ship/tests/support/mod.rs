//! Shared test support for the cloud sync family.
//!
//! Not a test target: `cargo test` compiles only the top-level `.rs` files in
//! `tests/`, so this module is pulled in with `mod support;` by each binary
//! that needs it.

use std::future::Future;
use std::time::Duration;

use wiremock::MockServer;

/// Fine enough that a wait costs about what the thing waited on costs (~2.4ms
/// for a local push), coarse enough not to spin. This is a POLL INTERVAL inside
/// a bounded condition, which is a different animal from a sleep used to
/// synchronise: shortening it makes a test slower to notice, never wrong.
const TICK: Duration = Duration::from_millis(2);

/// Poll `probe` until it reports ready, or give up after `budget`.
///
/// The primitive every readiness wait in these tests is built on. Its one
/// defining property: **the budget bounds FAILURE, never success.** A fast
/// machine returns in milliseconds; a slow or loaded one waits longer rather
/// than racing. That is the inversion a fixed sleep gets backwards — a sleep
/// pays in full whether or not it is needed, and then still loses if it guessed
/// low.
///
/// `label` names the condition, so a timeout reads as "the thing never
/// happened" rather than "the sleep was short".
pub async fn wait_until<F, Fut>(budget: Duration, label: &str, mut probe: F) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if probe().await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("{label} did not happen within {budget:?}"));
        }
        tokio::time::sleep(TICK).await;
    }
}

/// Wait until `server` has actually received a request for `path`, or give up.
///
/// # Why this is not a sleep
///
/// Every engine wired with `.with_cloud(..)` pushes FIRE-AND-FORGET: the
/// mutation returns immediately and a detached `tokio` task does the HTTP round
/// trip (`handle.spawn` in `roadmap/engine/mod.rs`, `signal/engine.rs`, and the
/// think/ship equivalents). So a test that mutates and asserts is racing its own
/// background task, and needs to wait for something.
///
/// The four cloud sync tests used to wait for a fixed 400ms, each carrying the
/// comment "give it a moment to land". That is a guess, and it was measured
/// against nothing. What the push actually costs, over 120 iterations each:
///
/// | condition            | p50    | p99    | max    |
/// |----------------------|--------|--------|--------|
/// | idle                 | 2.39ms | 2.70ms | 2.74ms |
/// | 8x CPU oversubscribed | 2.54ms | 33.1ms | 44.1ms |
///
/// So 400ms was roughly 160x the median and 9x the worst case that could be
/// manufactured locally — which sounds safe and is exactly the problem. Nobody
/// knows whether it is 100x too generous (it is, here) or 10x too small on a
/// slower runner, because the constant is unfalsifiable in both directions. It
/// was also not free: four tests x 400ms is 1.6s of pure sleeping in every
/// `cargo test` run. Replacing it with this condition made the same 120
/// iterations take 843ms instead of 48.2s.
///
/// # The property that makes a condition different from a sleep
///
/// **The budget bounds FAILURE, never success.** A fast machine returns in
/// milliseconds; a slow or loaded one waits longer rather than racing. That is
/// the inversion a fixed sleep gets backwards — it pays in full whether or not
/// it is needed, and then still loses if it guessed low.
///
/// # What it is not
///
/// It is not proof the wait is present. Deleting the call here costs only ~15%
/// of runs (18 failures in 120 measured with the wait removed), so a behavioural
/// suite cannot defend its own call sites. That job belongs to the structural
/// gate in `src/cloud/mod.rs`.
///
/// Returns `Err` with a diagnostic naming every path actually seen, so a
/// timeout reads as "the push never happened" rather than "the sleep was short".
pub async fn wait_for_request(
    server: &MockServer,
    path: &str,
    budget: Duration,
) -> Result<(), String> {
    let label = format!("a request for {path}");
    let Err(timeout) = wait_until(budget, &label, || async {
        server
            .received_requests()
            .await
            .is_some_and(|rs| rs.iter().any(|r| r.url.path() == path))
    })
    .await
    else {
        return Ok(());
    };

    // Name every path actually seen, so a timeout is diagnosable rather than
    // merely late.
    let seen = match server.received_requests().await {
        Some(rs) => {
            let paths: Vec<_> = rs.iter().map(|r| r.url.path().to_string()).collect();
            format!("saw {} request(s): {paths:?}", paths.len())
        }
        None => "wiremock request recording is off".to_string(),
    };
    Err(format!("{timeout}; {seen}"))
}

/// The helper's own gates. Compiled into every binary that pulls in `support`,
/// so each one proves the tool it depends on rather than assuming it.
#[cfg(test)]
mod tests {
    use super::*;

    /// THE VACUITY GATE. A readiness helper that returns success unconditionally
    /// looks IDENTICAL to a working one from every happy path — every cloud sync
    /// test would still pass, because the push it is waiting for lands anyway.
    /// Only a case where the awaited thing never happens can tell them apart.
    #[tokio::test]
    async fn a_wait_whose_condition_never_holds_fails_rather_than_succeeding() {
        let budget = Duration::from_millis(50);
        let started = tokio::time::Instant::now();
        let outcome = wait_until(budget, "a thing that never happens", || async { false }).await;

        let err = outcome.expect_err(
            "a wait whose condition never holds must fail; one that always returns Ok \
             is indistinguishable from a working wait on the happy path",
        );
        assert!(
            err.contains("a thing that never happens"),
            "the timeout must name its condition, got {err:?}",
        );
        assert!(
            started.elapsed() >= budget,
            "it must actually spend the budget before giving up, took {:?}",
            started.elapsed(),
        );
    }

    /// And the other direction: the budget bounds FAILURE, not success. A wait
    /// on a condition that already holds must return immediately rather than
    /// serving out its budget — which is the entire performance argument for
    /// replacing the fixed 400ms sleeps (48.2s of iterations became 843ms).
    #[tokio::test]
    async fn a_wait_whose_condition_already_holds_returns_without_spending_the_budget() {
        let budget = Duration::from_secs(30);
        let started = tokio::time::Instant::now();
        wait_until(budget, "a thing that already happened", || async { true })
            .await
            .expect("an already-true condition must resolve");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the budget bounds failure, not success — this waited {:?} of its {budget:?}",
            started.elapsed(),
        );
    }
}
