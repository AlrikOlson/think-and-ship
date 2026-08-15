//! Rate budget, accounted per provider AND per transport.
//!
//! # Why not one limiter
//!
//! GitHub bills REST and GraphQL from **separate** buckets: no more than 900
//! points per minute against REST endpoints and no more than 2,000 per minute
//! against the GraphQL endpoint, on top of the 5,000 requests/hour primary limit
//! for an authenticated user (docs.github.com, API version 2026-03-10). A single
//! shared limiter gets this wrong in both directions at once — it throttles
//! REST work because GraphQL was busy, and it reports headroom that does not
//! exist in the bucket actually being spent. Neither failure is visible from the
//! outside; you just see writes that are mysteriously slow, or 403s that the
//! limiter swore could not happen.
//!
//! So the key is `(provider, transport)`. Two providers never share, and within
//! one provider REST and GraphQL never share.
//!
//! # Honest scope
//!
//! The Issues adapter spends REST exclusively — GitHub Issues is a REST API.
//! The GraphQL bucket is exercised by tests only; its first real consumer is
//! the Projects v2 adapter, which is GraphQL-only. The separation is built now
//! because retrofitting it later means auditing every call site for which
//! bucket it charged.
//!
//! This is a *budget*, not a scheduler: it answers "may I spend n points, and if
//! not, for how long must I wait", and leaves the waiting to the caller. Adding
//! sleeping here would put a timer inside a type that every adapter holds, which
//! is how a rate limiter becomes impossible to test.

use std::collections::HashMap;
use std::time::Duration;

/// Which of a provider's separately-billed APIs a call spends from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Transport {
    Rest,
    GraphQl,
}

impl Transport {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::GraphQl => "graphql",
        }
    }
}

/// One bucket's allowance and what has been spent inside the current window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bucket {
    limit: u32,
    spent: u32,
    /// Monotonic tick the window opened at, in whatever unit the caller feeds
    /// [`RateBudget::advance`]. Kept caller-driven so tests are deterministic
    /// and this type never reads a clock.
    window_started: u64,
    window_len: u64,
}

/// The answer to "may I spend this?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spend {
    /// Charged. The bucket had room.
    Ok,
    /// Refused. The window must roll over first.
    Exhausted { retry_after: Duration },
}

/// Per-(provider, transport) point budgets.
///
/// The clock is injected via [`Self::advance`] rather than read, so a test can
/// prove window behaviour without sleeping and an adapter can drive it from
/// whatever time source it already has.
#[derive(Debug, Clone, Default)]
pub struct RateBudget {
    buckets: HashMap<(String, Transport), Bucket>,
    now: u64,
}

impl RateBudget {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// GitHub's documented secondary limits: 900 points/minute REST, 2,000
    /// points/minute GraphQL, from separate buckets.
    #[must_use]
    pub fn github() -> Self {
        let mut b = Self::new();
        b.configure("github", Transport::Rest, 900, 60);
        b.configure("github", Transport::GraphQl, 2_000, 60);
        b
    }

    /// Declare a bucket: `limit` points per `window_len` ticks.
    pub fn configure(&mut self, provider: &str, transport: Transport, limit: u32, window_len: u64) {
        self.buckets.insert(
            (provider.trim().to_ascii_lowercase(), transport),
            Bucket {
                limit,
                spent: 0,
                window_started: self.now,
                window_len,
            },
        );
    }

    /// Move the injected clock forward by `ticks` (seconds, by convention).
    pub fn advance(&mut self, ticks: u64) {
        self.now = self.now.saturating_add(ticks);
    }

    /// Charge `points` to one bucket.
    ///
    /// An unconfigured bucket is unlimited: a provider that has not declared its
    /// limits must not be silently throttled to zero, which would be the
    /// fail-closed choice in the one place it is wrong — the adapter, not this
    /// type, knows what its API charges.
    pub fn spend(&mut self, provider: &str, transport: Transport, points: u32) -> Spend {
        let now = self.now;
        let key = (provider.trim().to_ascii_lowercase(), transport);
        let Some(bucket) = self.buckets.get_mut(&key) else {
            return Spend::Ok;
        };
        if now.saturating_sub(bucket.window_started) >= bucket.window_len {
            bucket.window_started = now;
            bucket.spent = 0;
        }
        if bucket.spent.saturating_add(points) > bucket.limit {
            let elapsed = now.saturating_sub(bucket.window_started);
            return Spend::Exhausted {
                retry_after: Duration::from_secs(bucket.window_len.saturating_sub(elapsed)),
            };
        }
        bucket.spent += points;
        Spend::Ok
    }

    /// Points left in one bucket this window. `None` when unconfigured
    /// (unlimited).
    #[must_use]
    pub fn remaining(&self, provider: &str, transport: Transport) -> Option<u32> {
        let key = (provider.trim().to_ascii_lowercase(), transport);
        self.buckets.get(&key).map(|b| {
            if self.now.saturating_sub(b.window_started) >= b.window_len {
                b.limit
            } else {
                b.limit.saturating_sub(b.spent)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE property this type exists for. Spending the REST bucket to its limit
    /// must leave GraphQL completely untouched — a shared limiter would both
    /// throttle GraphQL work that had budget and hide that REST was exhausted.
    #[test]
    fn rest_and_graphql_are_accounted_separately() {
        let mut b = RateBudget::github();
        for _ in 0..900 {
            assert_eq!(b.spend("github", Transport::Rest, 1), Spend::Ok);
        }
        assert!(matches!(
            b.spend("github", Transport::Rest, 1),
            Spend::Exhausted { .. }
        ));

        assert_eq!(b.remaining("github", Transport::Rest), Some(0));
        assert_eq!(
            b.remaining("github", Transport::GraphQl),
            Some(2_000),
            "the GraphQL bucket must be untouched by REST spending"
        );
        assert_eq!(b.spend("github", Transport::GraphQl, 1), Spend::Ok);
    }

    /// Two providers must not share either, or connecting Linear would throttle
    /// GitHub.
    #[test]
    fn providers_do_not_share_a_bucket() {
        let mut b = RateBudget::new();
        b.configure("github", Transport::Rest, 2, 60);
        b.configure("linear", Transport::Rest, 2, 60);

        assert_eq!(b.spend("github", Transport::Rest, 2), Spend::Ok);
        assert!(matches!(
            b.spend("github", Transport::Rest, 1),
            Spend::Exhausted { .. }
        ));
        assert_eq!(b.remaining("linear", Transport::Rest), Some(2));
    }

    #[test]
    fn a_window_rollover_restores_the_allowance() {
        let mut b = RateBudget::new();
        b.configure("github", Transport::Rest, 2, 60);
        assert_eq!(b.spend("github", Transport::Rest, 2), Spend::Ok);

        let Spend::Exhausted { retry_after } = b.spend("github", Transport::Rest, 1) else {
            panic!("expected exhaustion");
        };
        assert_eq!(retry_after, Duration::from_secs(60));

        b.advance(60);
        assert_eq!(b.spend("github", Transport::Rest, 1), Spend::Ok);
    }

    /// Retry-after must shrink as the window elapses, or a caller that honours
    /// it waits a full window every time.
    #[test]
    fn retry_after_counts_down_within_the_window() {
        let mut b = RateBudget::new();
        b.configure("github", Transport::Rest, 1, 60);
        assert_eq!(b.spend("github", Transport::Rest, 1), Spend::Ok);
        b.advance(45);
        let Spend::Exhausted { retry_after } = b.spend("github", Transport::Rest, 1) else {
            panic!("expected exhaustion");
        };
        assert_eq!(retry_after, Duration::from_secs(15));
    }

    /// An adapter that never declared its limits must not be throttled to zero
    /// by default — this type does not know what any API charges.
    #[test]
    fn an_unconfigured_bucket_is_unlimited_not_closed() {
        let mut b = RateBudget::new();
        assert_eq!(b.spend("brand-new", Transport::Rest, 10_000), Spend::Ok);
        assert_eq!(b.remaining("brand-new", Transport::Rest), None);
    }

    /// A multi-point call must be refused whole rather than partially charged.
    #[test]
    fn an_oversized_spend_is_refused_without_charging() {
        let mut b = RateBudget::new();
        b.configure("github", Transport::Rest, 10, 60);
        assert_eq!(b.spend("github", Transport::Rest, 8), Spend::Ok);
        assert!(matches!(
            b.spend("github", Transport::Rest, 5),
            Spend::Exhausted { .. }
        ));
        assert_eq!(
            b.remaining("github", Transport::Rest),
            Some(2),
            "a refused spend must not consume anything"
        );
    }
}
