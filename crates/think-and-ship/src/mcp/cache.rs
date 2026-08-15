//! Cache metadata for cacheable results (SEP-2549).
//!
//! From MCP `2026-07-28`, `ttlMs` and `cacheScope` are **required** on every
//! `*/list` result and on `resources/read`. rmcp models them as `Option` so the
//! same types can carry results from older revisions, and it never fills them
//! in — which means a server that says nothing serializes neither field.
//!
//! That is the whole failure this module exists to prevent. rmcp's
//! `negotiate_protocol_version` echoes any version in the SDK's *global*
//! `KNOWN_VERSIONS`, so a client asking for `2026-07-28` gets `2026-07-28` back
//! whether or not we emit what that revision requires. A strict client then
//! rejects `tools/list` and `resources/list` outright and drops the entire tool
//! surface — the server looks healthy, the handshake succeeds, and not one tool
//! is reachable. rmcp's own client can't catch it: it deserializes both fields
//! as `Option` and shrugs at `None`.
//!
//! Two profiles, because this server serves two kinds of result:
//!
//! * [`catalog`] — the tool list and the resource catalog. Immutable for the
//!   lifetime of the process: the tool surface is compiled in, and the family
//!   selection that narrows it is fixed at startup. Shareable, because nothing
//!   in it varies by user.
//! * [`live_state`] — `resources/read`. The roadmap, the pinned steps and the
//!   digest are project state that any tool call can move, and the digest is
//!   additionally a function of wall-clock now. Never reusable, never shared.

use rmcp::model::CacheScope;

/// How long a catalog result stays fresh.
///
/// The catalog cannot change under a live connection, so the only staleness
/// this bounds is a cache that outlives the process — a client that kept the
/// list across an upgrade-and-reconnect. A minute keeps that self-healing
/// while still sparing a client the re-list it would otherwise do on every
/// poll. Longer buys nothing: clients list once per connection anyway.
pub const CATALOG_TTL_MS: u64 = 60_000;

/// Catalogs carry no per-user content — every client of a given server
/// instance is handed the identical list — so a shared cache in front of that
/// instance may serve it to anyone.
pub const CATALOG_SCOPE: CacheScope = CacheScope::Public;

/// Live state is stale the moment it is read: the next tool call can move the
/// roadmap or the trace, and a digest window is measured from *now*. Zero is
/// the honest answer, and per SEP-2549 it means "already stale".
pub const LIVE_STATE_TTL_MS: u64 = 0;

/// Roadmap chunks, reasoning steps and digests are the user's own project
/// state. Private keeps them out of any shared cache regardless of TTL.
pub const LIVE_STATE_SCOPE: CacheScope = CacheScope::Private;

/// Stamp a `*/list` result with the catalog cache profile.
///
/// Generic over the four `paginated_result!` types rather than written out
/// four times: they share no trait, but they do share the two builder methods,
/// so the `Cacheable` blanket below is what lets one call site cover them all.
pub fn catalog<T: Cacheable>(result: T) -> T {
    result.with_cache(CATALOG_TTL_MS, CATALOG_SCOPE)
}

/// Stamp a `resources/read` result with the live-state cache profile.
pub fn live_state<T: Cacheable>(result: T) -> T {
    result.with_cache(LIVE_STATE_TTL_MS, LIVE_STATE_SCOPE)
}

/// The SEP-2549 pair, on any result type that carries it.
///
/// rmcp generates `with_ttl_ms` / `with_cache_scope` on each result struct
/// independently, with no trait tying them together. This is that trait, so
/// [`catalog`] and [`live_state`] are single call sites instead of one
/// hand-written wrapper per result type — and so adding a fifth result type
/// later is one `impl` line rather than a new place to forget.
pub trait Cacheable {
    /// Set both fields. Consuming-self mirrors rmcp's own builders.
    fn with_cache(self, ttl_ms: u64, scope: CacheScope) -> Self;
}

macro_rules! impl_cacheable {
    ($($t:ty),+ $(,)?) => {
        $(
            impl Cacheable for $t {
                fn with_cache(self, ttl_ms: u64, scope: CacheScope) -> Self {
                    self.with_ttl_ms(ttl_ms).with_cache_scope(scope)
                }
            }
        )+
    };
}

impl_cacheable!(
    rmcp::model::ListToolsResult,
    rmcp::model::ListResourcesResult,
    rmcp::model::ListResourceTemplatesResult,
    rmcp::model::ListPromptsResult,
    rmcp::model::ReadResourceResult,
);

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{ListToolsResult, ReadResourceResult};

    /// The two fields are `skip_serializing_if = "Option::is_none"`, so
    /// "present in the JSON" and "`Some` on the struct" are the same claim.
    /// Asserting on the JSON is the one that matches what a client validates.
    #[test]
    fn catalog_serializes_both_required_fields() {
        let json = serde_json::to_value(catalog(ListToolsResult::with_all_items(vec![])))
            .expect("serialize");
        assert_eq!(json["ttlMs"], serde_json::json!(CATALOG_TTL_MS));
        assert_eq!(json["cacheScope"], serde_json::json!("public"));
    }

    #[test]
    fn live_state_serializes_both_required_fields() {
        let json =
            serde_json::to_value(live_state(ReadResourceResult::new(vec![]))).expect("serialize");
        assert_eq!(json["ttlMs"], serde_json::json!(0));
        assert_eq!(json["cacheScope"], serde_json::json!("private"));
    }

    /// Without a stamp there is no field at all — the exact shape that made a
    /// 2026-07-28 client discard all 53 tools. Pins the premise the two tests
    /// above rest on: they would pass just as happily if rmcp defaulted these.
    #[test]
    fn an_unstamped_result_carries_neither_field() {
        let json =
            serde_json::to_value(ListToolsResult::with_all_items(vec![])).expect("serialize");
        assert!(json.get("ttlMs").is_none(), "rmcp now defaults ttlMs");
        assert!(
            json.get("cacheScope").is_none(),
            "rmcp now defaults cacheScope"
        );
    }
}
