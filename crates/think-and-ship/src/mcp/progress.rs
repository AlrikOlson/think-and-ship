//! Liveness progress for long tool calls.
//!
//! Several tools do real work behind a single silent `tools/call`:
//! `ship_check` with a `command` runs an actual gate, `tracker push` walks a
//! network under a rate budget, the sweep reconciles, sync back-fills a corpus.
//! From outside, every one of them is byte-for-byte indistinguishable from a
//! wedged server. The failure that causes is not incorrectness — it is a human
//! losing confidence and killing a run that was fine.
//!
//! # What this emits, and what it deliberately does not
//!
//! A [`Heartbeat`] sends `notifications/progress` against the **caller's own**
//! `progressToken`, lifted from the request `_meta`. Each tick carries a
//! monotonically rising counter and a message naming the tool and the elapsed
//! seconds. It carries **no `total`**: a `tools/call` that shells out to
//! `cargo test` has no honest denominator, and the spec's own field doc says
//! progress "should increase every time progress is made, even if the total is
//! unknown". Inventing a percentage would be a fabricated number on a surface
//! whose entire purpose is trust.
//!
//! The tick is therefore a *liveness* signal, not a completion estimate. That
//! is also the thing a person waiting on a gate actually wants: not "how far",
//! but "still alive, and here is how long it has been".
//!
//! # Why the first tick is delayed
//!
//! Measured on this tree, `cargo test --workspace` takes 7.5s fully warm, 24.1s
//! with one source file touched, and ~90s cold. Meanwhile the overwhelming
//! majority of this server's ~48 tools return in single-digit milliseconds.
//! Ticking from t=0 would spray a notification at every trivial call for no
//! human benefit. [`FIRST_TICK`] is what makes the feature *silent* in the
//! common case while still firing several times for even the fastest real gate.
//!
//! # Degrade, never fail
//!
//! Three independent silent degradations, none of which is an error path and
//! none of which changes the tool's result by a single byte:
//!
//! 1. **No `progressToken` in the request `_meta`** — [`Heartbeat::start`]
//!    returns an inert handle and spawns nothing.
//! 2. **The notification cannot be delivered** (client gone, transport closed,
//!    client ignores progress) — the tick loop stops and stays stopped.
//! 3. **The call finishes before [`FIRST_TICK`]** — the handle is dropped, the
//!    task is aborted, and not one notification was ever sent.
//!
//! # Composition with SEP-2663 tasks (`crate::mcp::tasks`)
//!
//! Recorded here in writing because tasks and progress are complements
//! that are easy to mistake for duplicates. **Tasks change how long the call
//! stays open; progress changes what the human sees while it is open.** The
//! tasks side must *not* rebuild emission: a task-augmented
//! `ship_check` still receives the caller's `progressToken` in the same request
//! `_meta`, so it should hold this same [`Heartbeat`] across the task's
//! lifetime and let the augmented tick text name the task id. The one thing
//! that must not happen is a second, parallel progress mechanism hanging off
//! `tasks/update`, which would double every notification.

use std::time::Duration;

use rmcp::{
    Peer, RoleServer,
    model::{ProgressNotificationParam, ProgressToken, RequestMetaObject},
};

/// How long a call must run before its first liveness tick.
///
/// Above the fast-call population (single-digit ms) and below the floor of the
/// slow one (7.5s warm `cargo test`), so trivial calls stay silent while every
/// real gate still reports.
pub const FIRST_TICK: Duration = Duration::from_secs(2);

/// Spacing between ticks once they start. Bounds a 90s gate to ~44 tiny
/// notifications.
pub const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// The message body of the `n`-th tick (1-based) for `tool`.
///
/// Split out as a pure function so the wording is assertable by exact value
/// rather than by "a message was present" — a shape check would pass on an
/// empty string, which is exactly the notification a human learns nothing from.
#[must_use]
pub fn tick_message(tool: &str, tick: u64) -> String {
    format!(
        "{tool} still running ({}s elapsed)",
        elapsed_secs_at_tick(tick)
    )
}

/// Wall-clock seconds a caller has waited when tick `n` (1-based) fires.
///
/// The first tick lands at [`FIRST_TICK`]; each later one adds
/// [`TICK_INTERVAL`].
#[must_use]
pub fn elapsed_secs_at_tick(tick: u64) -> u64 {
    FIRST_TICK.as_secs() + TICK_INTERVAL.as_secs() * tick.saturating_sub(1)
}

/// The caller's `progressToken`, or `None` when it sent none.
///
/// The whole of degradation 1 lives in this `Option`. It is a free function
/// rather than an inline `let else` so the "no token → no ticking" decision can
/// be asserted without constructing a live `Peer`, which a unit test cannot do.
#[must_use]
pub fn token_of(meta: &RequestMetaObject) -> Option<ProgressToken> {
    meta.get_progress_token()
}

/// A running liveness ticker, stopped when dropped.
///
/// [`Heartbeat::start`] returns an inert instance when the caller supplied no
/// `progressToken` — the common case, and the reason this type has no fallible
/// constructor.
#[derive(Debug)]
pub struct Heartbeat {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Heartbeat {
    /// A heartbeat that never ticks. Used when there is no token to tick
    /// against, and directly assertable in tests.
    #[must_use]
    pub fn inert() -> Self {
        Self { task: None }
    }

    /// True when this heartbeat will never emit anything.
    #[must_use]
    pub fn is_inert(&self) -> bool {
        self.task.is_none()
    }

    /// Start ticking against the caller's `progressToken`, if it sent one.
    ///
    /// Returns [`Heartbeat::inert`] when `meta` carries no token, which is the
    /// whole of degradation 1: no token, no task, no notifications, and a tool
    /// result identical to what the caller would have received before this
    /// module existed.
    #[must_use]
    pub fn start(peer: Peer<RoleServer>, meta: &RequestMetaObject, tool: &str) -> Self {
        let Some(token) = token_of(meta) else {
            return Self::inert();
        };
        Self::start_with_token(peer, token, tool)
    }

    /// The ticking half, separated from token extraction so a test can drive it
    /// without constructing a `RequestMetaObject`.
    #[must_use]
    pub fn start_with_token(peer: Peer<RoleServer>, token: ProgressToken, tool: &str) -> Self {
        let tool = tool.to_string();
        let task = tokio::spawn(async move {
            tokio::time::sleep(FIRST_TICK).await;
            let mut tick: u64 = 0;
            loop {
                tick += 1;
                let param = ProgressNotificationParam::new(token.clone(), tick as f64)
                    .with_message(tick_message(&tool, tick));
                // Delivery failure is not this server's problem to report: the
                // tool call itself is unaffected, so the loop simply stops.
                if peer.notify_progress(param).await.is_err() {
                    return;
                }
                tokio::time::sleep(TICK_INTERVAL).await;
            }
        });
        Self { task: Some(task) }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_tick_is_above_the_fast_call_population_and_below_the_slow_one() {
        // Not a taste check: 2s is the measured boundary between the ~48 tools
        // that return in milliseconds and the 7.5s floor of a warm gate.
        assert_eq!(FIRST_TICK, Duration::from_secs(2));
        assert!(FIRST_TICK < Duration::from_millis(7500));
    }

    #[test]
    fn elapsed_seconds_are_exact_at_each_tick() {
        assert_eq!(elapsed_secs_at_tick(1), 2);
        assert_eq!(elapsed_secs_at_tick(2), 4);
        assert_eq!(elapsed_secs_at_tick(3), 6);
        assert_eq!(elapsed_secs_at_tick(45), 90);
        // A 0th tick cannot happen, but saturating_sub must not underflow.
        assert_eq!(elapsed_secs_at_tick(0), 2);
    }

    #[test]
    fn tick_message_names_the_tool_and_the_elapsed_time() {
        assert_eq!(
            tick_message("ship_check", 1),
            "ship_check still running (2s elapsed)"
        );
        assert_eq!(
            tick_message("ship_check", 45),
            "ship_check still running (90s elapsed)"
        );
    }

    #[test]
    fn a_request_without_a_progress_token_yields_no_token_to_tick_against() {
        // Degradation 1, at the only level it CAN be tested: rmcp's own client
        // sets a progressToken on every outbound request unconditionally
        // (service.rs:800), so no real client can produce this case over the
        // wire. `token_of` returning None is exactly what makes `start` inert.
        assert!(token_of(&RequestMetaObject::new()).is_none());
    }

    #[test]
    fn a_request_carrying_a_progress_token_yields_that_exact_token() {
        // The twin of the test above. Without it, `token_of` could return None
        // unconditionally — degrading everything to silence — and the negative
        // test alone would still pass.
        let mut meta = RequestMetaObject::new();
        let token = ProgressToken(rmcp::model::NumberOrString::String("tok-7".into()));
        meta.set_progress_token(token.clone());
        assert_eq!(token_of(&meta), Some(token));
    }

    #[test]
    fn an_inert_heartbeat_reports_itself_inert() {
        assert!(Heartbeat::inert().is_inert());
    }
}
