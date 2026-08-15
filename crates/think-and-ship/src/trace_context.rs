//! W3C Trace Context adoption (MCP SEP-414) — stop being an island.
//!
//! [`crate::otel`] exports a self-contained trace whose ids are sha256-derived
//! from our own record identities. That is deterministic and reproducible, and
//! it is also unjoinable: a trace that starts in a host application could never
//! contain our spans, because our trace id was never the host's trace id.
//!
//! SEP-414 fixes that by carrying W3C Trace Context in a request's `_meta`
//! under the reserved keys `traceparent` / `tracestate` / `baggage`. rmcp
//! 3.0.0-beta.2 already implements the wire half — it declares those exact key
//! names and lifts `params._meta` into `RequestContext::meta` before dispatch —
//! so this module is the half that is ours: parse the value the spec's way,
//! remember it across processes, and let the exporter parent to it.
//!
//! Cross-process is the whole difficulty. The MCP server adopts the context,
//! but `think-and-ship trace export` runs later in a *different*
//! process. Both sides resolve the same project id from the working directory,
//! so the adopted context is persisted under that key and read back by the
//! exporter. Persistence obeys `THINK_AND_SHIP_PERSIST` like every other
//! family; with it off, adoption is a no-op and the export is what it always
//! was.
//!
//! The join rule: **the caller owns the trace id, we keep our span
//! ids.** Adopting the caller's trace id is what makes the trees one; span ids
//! only need to be unique within a trace, so regenerating them would buy
//! nothing and would cost the determinism the exporter was built for.

use serde::{Deserialize, Serialize};

use crate::infra::{Domain, Persistence, PersistenceConfig};

/// A caller's trace context, parsed and validated, as adopted from `_meta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundTrace {
    /// 32 lowercase hex chars — the caller's trace id. Ours becomes this.
    pub trace_id: String,
    /// 16 lowercase hex chars — the caller's span id. Our root parents to it.
    pub parent_span_id: String,
    /// The `sampled` bit of trace-flags. Recorded, not yet acted on.
    pub sampled: bool,
    /// Vendor state, passed through verbatim. Never parsed — the spec says a
    /// vendor that does not understand an entry must preserve it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tracestate: Option<String>,
    /// W3C Baggage, passed through verbatim.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub baggage: Option<String>,
    /// RFC3339 instant this context was adopted.
    pub adopted_at: String,
}

/// A `traceparent` field is exactly two lowercase hex digits, or 32, or 16.
/// Uppercase is invalid: the spec's own illustration of an invalid parent-id is
/// one "containing non-lowercase hex characters".
fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn all_zero(s: &str) -> bool {
    s.bytes().all(|b| b == b'0')
}

/// Parse a W3C `traceparent` into `(trace_id, parent_span_id, sampled)`.
///
/// Returns `None` for anything the spec says a vendor MUST ignore, which is
/// the entire error strategy here: an unparseable context is *absence*, never
/// a failed tool call.
///
/// Enforced: `version-traceid-parentid-flags`; version is two lowercase hex
/// digits and never `ff` (reserved); trace-id is 32 lowercase hex and not all
/// zeros; parent-id is 16 lowercase hex and not all zeros; flags are two
/// lowercase hex digits. Version `00` must have *exactly* four fields — a
/// trailing field is invalid at that version. Higher versions are parsed
/// leniently from their first four fields, which is the spec's forward
/// compatibility rule, so a future sender is joined rather than dropped.
pub fn parse_traceparent(raw: &str) -> Option<(String, String, bool)> {
    let parts: Vec<&str> = raw.split('-').collect();
    if parts.len() < 4 {
        return None;
    }
    let (version, trace, parent, flags) = (parts[0], parts[1], parts[2], parts[3]);
    if !is_lower_hex(version, 2) || version == "ff" {
        return None;
    }
    // Version 00 is fully specified: four fields, no more.
    if version == "00" && parts.len() != 4 {
        return None;
    }
    if !is_lower_hex(trace, 32) || all_zero(trace) {
        return None;
    }
    if !is_lower_hex(parent, 16) || all_zero(parent) {
        return None;
    }
    if !is_lower_hex(flags, 2) {
        return None;
    }
    let sampled = u8::from_str_radix(flags, 16).ok()? & 0x01 != 0;
    Some((trace.to_string(), parent.to_string(), sampled))
}

impl InboundTrace {
    /// Build from the three `_meta` values, as rmcp hands them over. `None`
    /// when there is no usable `traceparent` — `tracestate` and `baggage` are
    /// meaningless without one and are dropped with it.
    pub fn from_meta(
        traceparent: Option<&str>,
        tracestate: Option<&str>,
        baggage: Option<&str>,
        adopted_at: String,
    ) -> Option<Self> {
        let (trace_id, parent_span_id, sampled) = parse_traceparent(traceparent?)?;
        Some(Self {
            trace_id,
            parent_span_id,
            sampled,
            tracestate: tracestate.map(str::to_string),
            baggage: baggage.map(str::to_string),
            adopted_at,
        })
    }

    /// The `traceparent` to send on an outbound request made under this
    /// context, naming `our_span_id` as the parent of the downstream hop.
    ///
    /// Always version `00`, because that is the version we understand. The
    /// sampled bit is carried through unchanged: a caller that is not sampling
    /// does not become sampled by passing through us.
    ///
    /// # Why this takes a span id instead of re-rendering the caller's
    ///
    /// This replaced a `to_traceparent()` that rendered
    /// `00-{trace_id}-{parent_span_id}-{flags}` — the caller's own header,
    /// verbatim. That is the PASS-THROUGH option, and it was rejected (see the
    /// module-level decision below), so the function it existed for was never
    /// going to be written. It had zero callers for exactly that reason.
    pub fn child_traceparent(&self, our_span_id: &str) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id,
            our_span_id,
            u8::from(self.sampled)
        )
    }
}

/// The `traceparent` header value for an outbound HTTP call made on this
/// project's behalf, or `None` when there is no caller context to be a child
/// of.
///
/// `None` is the common case and the safe one: no adopted context means no
/// header, never a fabricated root. A trace that did not start somewhere else
/// must not appear to have.
///
/// # The decision: MINT a child span id, do not pass the caller's through
///
/// The W3C spec is normative on the obligation and permissive on the shape:
/// "A vendor receiving a `traceparent` request header MUST send it to outgoing
/// requests. It MAY mutate the value of this header before passing it to
/// outgoing requests." So propagating is not optional; what we put in
/// `parent-id` is ours to choose. Three candidates, in the order they were
/// considered:
///
/// **(a) Pass the caller's `parent-id` through unchanged.** Simplest, and
/// spec-legal — mutation is a MAY. Rejected because it is a lie about who made
/// the call: the Linear/GitHub/Worker leg would parent to the HOST's span,
/// making it a SIBLING of our own root rather than a child of it. The tree
/// renders flat, and a reader would conclude the host called Linear directly.
///
/// **(b) Mint a FRESH span id per outbound call.** What a real tracer does,
/// and the reason it is a real tracer: it also EMITS that span. We do not —
/// live span emission is a separate, deliberately deferred piece of work
/// (`otel-live-span-emission`). A per-call id would name a parent that nothing
/// ever emits, which is not a nested tree but a broken one: a dangling
/// `parentSpanId` renders as an orphan. Rejected as the option that looks most
/// correct and is least honest about what we actually publish.
///
/// **(c) CHOSEN — name the span the offline export already emits.** Our export
/// root is [`crate::otel::span_id`]`("workspace", project)`, and when a caller
/// context is adopted that root carries the caller's trace id and parents to
/// the caller's span. Pointing the downstream hop at that same id makes the
/// Linear/GitHub/Worker leg a child of OUR workspace span, which is true, and
/// the parent is not dangling: it arrives the moment
/// `think-and-ship trace export --otel` is POSTed. The two id policies do not
/// have to be reconciled because they were never in conflict — the
/// deterministic sha256 id is what makes this joinable at all. Nothing about
/// the export changes: [`crate::otel::span_id`] is a pure function being READ
/// here, so byte-stability is untouched.
///
/// # What is deliberately NOT propagated
///
/// `tracestate` and `baggage` are adopted and persisted, and they stay here.
/// Both are free-form, caller-controlled values, and Linear and GitHub are
/// outside this project's trust boundary — forwarding them is a data egress
/// the caller never asked for. A per-destination policy (send them to our own
/// Worker, withhold them from third parties) would be defensible and is
/// rejected on a different ground: it is a rule nobody would remember when
/// adding the fourth provider. `traceparent` alone carries everything needed
/// to join the tree.
#[must_use]
pub fn outbound_traceparent(project: &str) -> Option<String> {
    outbound_traceparent_from(&store_handle(), project)
}

/// [`outbound_traceparent`] against an explicit handle. Same reasoning as
/// [`load_from`]: the env-reading wrapper cannot be pointed at a scratch
/// directory without mutating process-global environment, so the derivation a
/// test needs to pin lives here.
pub(crate) fn outbound_traceparent_from(store: &Persistence, project: &str) -> Option<String> {
    let adopted = load_from(store, project)?;
    Some(adopted.child_traceparent(&crate::otel::span_id("workspace", project)))
}

fn store_handle() -> Persistence {
    Persistence::new(&PersistenceConfig::from_env(), Domain::Otel)
}

/// Remember `trace` as the project's most recent caller context.
///
/// Last-writer-wins by design: the exporter wants the newest tree to hang
/// under, not a merge of every caller that ever called. No-op when persistence
/// is disabled.
pub fn store(project: &str, trace: &InboundTrace) -> std::io::Result<()> {
    store_into(&store_handle(), project, trace)
}

/// The project's most recent adopted caller context, if any.
///
/// Deliberately total: a missing file, persistence being off, or a store
/// written by a different schema all mean "no caller context", which is the
/// unjoined export we shipped before this existed.
pub fn load(project: &str) -> Option<InboundTrace> {
    load_from(&store_handle(), project)
}

/// [`store`] against an explicit handle. The env-reading wrappers above cannot
/// be pointed at a scratch directory without mutating process-global
/// environment, so the real read/write policy lives here where a test can
/// reach it.
pub(crate) fn store_into(
    store: &Persistence,
    project: &str,
    trace: &InboundTrace,
) -> std::io::Result<()> {
    store.save(project, trace)
}

/// [`load`] against an explicit handle. See [`store_into`].
pub(crate) fn load_from(store: &Persistence, project: &str) -> Option<InboundTrace> {
    store.load(project).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01";

    #[test]
    fn parses_the_spec_example() {
        let (trace, parent, sampled) = parse_traceparent(VALID).expect("spec example must parse");
        assert_eq!(trace, "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(parent, "00f067aa0ba902b7");
        assert!(sampled);
    }

    #[test]
    fn unsampled_flag_is_read_not_assumed() {
        let raw = "00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-00";
        let (_, _, sampled) = parse_traceparent(raw).expect("must parse");
        assert!(!sampled, "trace-flags 00 means not sampled");
    }

    #[test]
    fn rejects_uppercase_hex() {
        // The spec's own example of an invalid parent-id.
        let raw = "00-0AF7651916CD43DD8448EB211C80319C-00f067aa0ba902b7-01";
        assert!(parse_traceparent(raw).is_none());
    }

    #[test]
    fn rejects_all_zero_trace_id() {
        let raw = "00-00000000000000000000000000000000-00f067aa0ba902b7-01";
        assert!(parse_traceparent(raw).is_none());
    }

    #[test]
    fn rejects_all_zero_parent_id() {
        let raw = "00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01";
        assert!(parse_traceparent(raw).is_none());
    }

    #[test]
    fn rejects_reserved_version_ff() {
        let raw = "ff-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01";
        assert!(parse_traceparent(raw).is_none());
    }

    #[test]
    fn rejects_version_00_with_a_trailing_field() {
        let raw = "00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01-extra";
        assert!(parse_traceparent(raw).is_none());
    }

    #[test]
    fn accepts_a_future_version_with_extra_fields() {
        // Forward compatibility: a later version is joined, not dropped.
        let raw = "01-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01-whatever";
        let (trace, _, _) = parse_traceparent(raw).expect("future versions parse leniently");
        assert_eq!(trace, "0af7651916cd43dd8448eb211c80319c");
    }

    #[test]
    fn rejects_short_and_empty_input() {
        assert!(parse_traceparent("").is_none());
        assert!(parse_traceparent("00-0af7651916cd43dd8448eb211c80319c").is_none());
        assert!(parse_traceparent("garbage").is_none());
    }

    #[test]
    fn from_meta_drops_siblings_when_traceparent_is_unusable() {
        let got = InboundTrace::from_meta(
            Some("nonsense"),
            Some("vendor=1"),
            Some("user=alice"),
            "2026-07-27T00:00:00Z".into(),
        );
        assert!(
            got.is_none(),
            "tracestate/baggage are meaningless without a valid traceparent"
        );
    }

    #[test]
    fn from_meta_passes_siblings_through_verbatim() {
        let got = InboundTrace::from_meta(
            Some(VALID),
            Some("vendor1=value1,vendor2=value2"),
            Some("userId=alice"),
            "2026-07-27T00:00:00Z".into(),
        )
        .expect("must adopt");
        assert_eq!(
            got.tracestate.as_deref(),
            Some("vendor1=value1,vendor2=value2")
        );
        assert_eq!(got.baggage.as_deref(), Some("userId=alice"));
    }

    /// The claim the whole chunk rests on: the MCP server stores a context in
    /// one process and `trace export` reads it back in another. If this
    /// is wrong the join silently never happens and everything else here is
    /// decoration.
    #[test]
    fn survives_the_process_boundary_via_the_otel_partition() {
        let dir = std::env::temp_dir().join(format!(
            "tas-trace-ctx-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let cfg = PersistenceConfig::from_env()
            .enabled(true)
            .with_data_dir(dir.clone());
        let store = Persistence::new(&cfg, Domain::Otel);

        let adopted = InboundTrace::from_meta(
            Some(VALID),
            Some("v=1"),
            None,
            "2026-07-27T00:00:00Z".into(),
        )
        .expect("must adopt");
        store_into(&store, "proj-abc123", &adopted).expect("save");

        // A *fresh* handle, as a later process would build.
        let reader = Persistence::new(&cfg, Domain::Otel);
        assert_eq!(load_from(&reader, "proj-abc123").as_ref(), Some(&adopted));

        // Project-scoped: another project must not see this context.
        assert!(
            load_from(&reader, "proj-other").is_none(),
            "contexts must not bleed across projects"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Adoption is opt-in with the same switch as every other family. With
    /// persistence off, `store` must not write and `load` must find nothing.
    #[test]
    fn adoption_is_a_no_op_when_persistence_is_disabled() {
        let dir = std::env::temp_dir().join(format!("tas-trace-ctx-off-{}", std::process::id()));
        let cfg = PersistenceConfig::from_env()
            .enabled(false)
            .with_data_dir(dir.clone());
        let store = Persistence::new(&cfg, Domain::Otel);
        let adopted =
            InboundTrace::from_meta(Some(VALID), None, None, "t".into()).expect("must adopt");
        store_into(&store, "proj-abc123", &adopted).expect("save is a no-op");
        assert!(
            load_from(&store, "proj-abc123").is_none(),
            "disabled persistence must store nothing"
        );
    }

    /// The rejected PASS-THROUGH option, asserted as a NON-property. If the
    /// downstream header ever equals the caller's own header, option (a) has
    /// crept back in and the Linear/GitHub leg has become a sibling of our
    /// root instead of a child of it.
    #[test]
    fn the_child_header_is_not_the_callers_header() {
        let got = InboundTrace::from_meta(Some(VALID), None, None, "t".into()).expect("must adopt");
        let child = got.child_traceparent(&crate::otel::span_id("workspace", "proj-abc123"));
        assert_ne!(child, VALID, "pass-through was the REJECTED option");
        // The trace id — the thing that makes the trees one — is preserved.
        assert!(child.starts_with(&format!("00-{}-", got.trace_id)));
        // The parent-id is ours, not theirs.
        assert!(!child.contains(&got.parent_span_id));
    }

    /// The sampled bit is the caller's decision. A caller that is not sampling
    /// must not become sampled by passing through us.
    #[test]
    fn the_child_header_carries_the_callers_sampling_decision() {
        let unsampled = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00";
        let got =
            InboundTrace::from_meta(Some(unsampled), None, None, "t".into()).expect("must adopt");
        assert!(got.child_traceparent("aaaaaaaaaaaaaaaa").ends_with("-00"));

        let got = InboundTrace::from_meta(Some(VALID), None, None, "t".into()).expect("must adopt");
        assert!(got.child_traceparent("aaaaaaaaaaaaaaaa").ends_with("-01"));
    }

    /// THE JOIN, asserted as the property that makes the tree close rather
    /// than dangle: the parent-id we send downstream must be a span id the
    /// offline export actually EMITS. Option (b) — a fresh per-call id —
    /// passes every "is it well-formed" check and fails this one.
    #[test]
    fn the_downstream_parent_is_the_span_the_export_emits() {
        let dir = std::env::temp_dir().join(format!(
            "tas-trace-out-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let cfg = PersistenceConfig::from_env()
            .enabled(true)
            .with_data_dir(dir.clone());
        let store = Persistence::new(&cfg, Domain::Otel);

        // No adopted context: no header. Never a fabricated root.
        assert!(
            outbound_traceparent_from(&store, "proj-abc123").is_none(),
            "an unparented call must not acquire a root"
        );

        let adopted =
            InboundTrace::from_meta(Some(VALID), None, None, "t".into()).expect("must adopt");
        store_into(&store, "proj-abc123", &adopted).expect("save");

        let header = outbound_traceparent_from(&store, "proj-abc123").expect("context adopted");
        let parts: Vec<&str> = header.split('-').collect();
        assert_eq!(parts.len(), 4, "well-formed traceparent");
        // Same trace as the caller — this is what joins the trees.
        assert_eq!(parts[1], adopted.trace_id);
        // And the parent is the export's ROOT span for this project, which
        // `build_export` emits under exactly this trace id when joined.
        assert_eq!(parts[2], crate::otel::span_id("workspace", "proj-abc123"));

        // Still project-scoped: another project derives a different parent.
        assert!(outbound_traceparent_from(&store, "proj-other").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
