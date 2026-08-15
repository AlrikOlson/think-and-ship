//! What the client on the other end of *this request* declared.
//!
//! # Why this module exists
//!
//! Two shipped features announce which capability lane they took by printing
//! to stderr: [`crate::mcp::tasks::Eligibility::decide`] and
//! [`crate::mcp::elicit`]. Claude Code captures a server's stderr exactly
//! once, at connect time, and discards everything after — verified across five
//! session logs. So through the host we actually use, those lines
//! are unreadable, and "is this feature live?" became a question answerable
//! only by timing experiments.
//!
//! The stderr lines **stay**. They are a *push* channel for an operator
//! watching a process, they fire at the instant a decision is taken, and they
//! are the only thing that exists when the server runs outside a client at
//! all. A line is removed when it is wrong, not when one host cannot read it.
//! This module is the *pull* channel beside them: what is true right now, on
//! the connection you are asking from.
//!
//! # The shape, and why nothing is cached
//!
//! [`ClientView::observe`] takes the request's own `RequestContext` and reads
//! it. Nothing is captured at the `UnifiedService::call_tool` seam, nothing is
//! stored, and there is no snapshot — so there is no staleness mode, no second
//! writer, and no cache to invalidate. A written-down copy would reproduce the
//! very bug this module exists to fix, one medium over. The answer cannot be
//! out of date because it is read from the connection that asked.
//!
//! # Two honest asymmetries this deliberately makes visible
//!
//! 1. **Capabilities have two sources.** `RequestContext::client_capabilities`
//!    consults the per-request `_meta` first (the pre-handshake SEP path) and
//!    falls back to the `initialize` handshake. A capability report that does
//!    not say *where it got the capability from* repeats the failure mode of
//!    the stderr lines — authoritative-looking and unlocatable. So
//!    [`ClientView::capability_source`] names the branch.
//! 2. **`supported_elicitation_modes` reads `initialize` only.** rmcp's
//!    accessor goes through `peer_info()` and never looks at `_meta`, while
//!    `client_capabilities()` looks at both. A client that declared
//!    elicitation *only* through `_meta` therefore shows
//!    `declares_elicitation: true` with an **empty** `elicitation_modes` — and
//!    since [`crate::mcp::elicit`] gates on the modes, that client would never
//!    be asked. Reporting both fields side by side is what makes that
//!    divergence visible instead of mysterious.
//!
//! # What is deliberately NOT here
//!
//! No derived `may_ask` boolean. The real predicate in
//! [`crate::mcp::elicit::ask_propose_consent`] has three gates — the env
//! switch, the client's `Form` mode, and a remembered prior answer on disk —
//! and a two-gate conjunction would read as an answer while being wrong for
//! anyone who already answered. The atomic facts are reported and the reader
//! combines them. A field that cannot be wrong beats a field that is usually
//! right.

use rmcp::{
    RoleServer,
    model::{ClientCapabilities, Implementation, ProtocolVersion},
    service::RequestContext,
};
use schemars::JsonSchema;
use serde::Serialize;

/// Capabilities came from the per-request `_meta`, the pre-handshake path.
pub const SOURCE_REQUEST_META: &str = "request_meta";
/// Capabilities came from the `initialize` handshake.
pub const SOURCE_INITIALIZE: &str = "initialize";
/// No declaration was VISIBLE at all — neither a handshake nor a request
/// `_meta`. This is not the same as "declared nothing": a client that completed
/// initialize with an empty capabilities object reports [`SOURCE_INITIALIZE`]
/// with every capability false. `none` means the request could not see one,
/// which is the case worth investigating.
pub const SOURCE_NONE: &str = "none";

/// Which branch a request's capabilities came out of.
///
/// Split out of [`ClientView::observe`] as a pure function on purpose: the
/// derivation is the one piece of logic in this module that a wire test cannot
/// drive, because sending capabilities through per-request `_meta` needs a
/// client we do not have. Left inline it would be an assertion nobody can make
/// — the exact vacuity this module exists to stop shipping.
///
/// The two inputs are read from the SAME accessor pair the capabilities come
/// from, so source and capabilities cannot disagree: rmcp lets `_meta` win
/// whenever it is present, so its presence IS the branch. rmcp's own
/// `request_metadata_required()` is `pub(crate)` and is not needed — when
/// metadata is required and absent, `client_capabilities()` is `None`, which
/// lands on [`SOURCE_NONE`] correctly.
#[must_use]
pub fn source_of(declared_in_request_meta: bool, has_capabilities: bool) -> &'static str {
    if declared_in_request_meta {
        SOURCE_REQUEST_META
    } else if has_capabilities {
        SOURCE_INITIALIZE
    } else {
        SOURCE_NONE
    }
}

/// The live client, as declared to *this* request.
///
/// Every field is an atomic observation. Nothing here is inferred from
/// behaviour and nothing is remembered between calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ClientView {
    /// The client's self-reported name, e.g. `claude-code`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The client's self-reported version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The MCP protocol version in force for this request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    /// Where the capabilities below were read from: `request_meta`,
    /// `initialize`, or `none`. See the module's asymmetry (1).
    pub capability_source: String,
    /// Whether the client declared the elicitation capability at all.
    pub declares_elicitation: bool,
    /// The elicitation modes rmcp will actually honour — read from the
    /// `initialize` handshake ONLY. Can be empty while `declares_elicitation`
    /// is true; see the module's asymmetry (2). `crate::mcp::elicit` gates on
    /// `form` being present here, not on `declares_elicitation`.
    pub elicitation_modes: Vec<String>,
    /// Whether the client declared the SEP-2663 tasks extension. This is the
    /// direct read of a fact that previously could only be established by
    /// timing a six-second gate against a control.
    pub declares_tasks: bool,
    /// Whether the client declared LLM sampling.
    pub declares_sampling: bool,
    /// Whether the client declared filesystem roots.
    pub declares_roots: bool,
    /// Every declared MCP extension id, sorted.
    pub extensions: Vec<String>,
    /// Whether `THINK_AND_SHIP_ELICIT` is on — the first and only gate no
    /// property of the client can reach past. Reported beside the client's own
    /// declaration because the question "would we ask?" needs both, plus the
    /// remembered answer that is deliberately not folded in here.
    pub asking_enabled: bool,
}

impl ClientView {
    /// Read the live client off the request that asked.
    ///
    /// Impure only in the two ways it must be: it touches the peer and it
    /// reads the env gate. Everything else is [`ClientView::describe`], which
    /// a test can drive with no wire at all.
    #[must_use]
    pub fn observe(context: &RequestContext<RoleServer>) -> Self {
        let capabilities = context.client_capabilities();
        let source = source_of(
            context.meta.client_capabilities().is_some(),
            capabilities.is_some(),
        );
        let mut modes: Vec<String> = context
            .peer
            .supported_elicitation_modes()
            .iter()
            .map(|m| format!("{m:?}").to_lowercase())
            .collect();
        modes.sort();
        Self::describe(
            context.client_info(),
            context.protocol_version(),
            capabilities.as_ref(),
            source,
            modes,
            crate::mcp::elicit::asking_enabled(),
        )
    }

    /// The pure half: everything above, with the wire and the env already
    /// read. Split off so every claim about what a declaration *means* is
    /// assertable without a live client.
    #[must_use]
    pub fn describe(
        client_info: Option<Implementation>,
        protocol_version: Option<ProtocolVersion>,
        capabilities: Option<&ClientCapabilities>,
        capability_source: &str,
        elicitation_modes: Vec<String>,
        asking_enabled: bool,
    ) -> Self {
        let mut extensions: Vec<String> = capabilities
            .and_then(|c| c.extensions.as_ref())
            .map(|e| e.keys().cloned().collect())
            .unwrap_or_default();
        extensions.sort();
        Self {
            name: client_info.as_ref().map(|i| i.name.clone()),
            version: client_info.map(|i| i.version),
            protocol_version: protocol_version.map(|v| v.to_string()),
            capability_source: capability_source.to_string(),
            declares_elicitation: capabilities.is_some_and(|c| c.elicitation.is_some()),
            elicitation_modes,
            declares_tasks: capabilities.is_some_and(ClientCapabilities::supports_tasks),
            declares_sampling: capabilities.is_some_and(|c| c.sampling.is_some()),
            declares_roots: capabilities.is_some_and(|c| c.roots.is_some()),
            extensions,
            asking_enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::TASKS_EXTENSION_ID;

    fn caps() -> ClientCapabilities {
        ClientCapabilities::default()
    }

    #[test]
    fn a_client_that_declared_nothing_reports_every_capability_false() {
        let v = ClientView::describe(None, None, None, SOURCE_NONE, vec![], false);
        assert!(!v.declares_elicitation);
        assert!(!v.declares_tasks);
        assert!(!v.declares_sampling);
        assert!(!v.declares_roots);
        assert!(
            v.extensions.is_empty(),
            "no capabilities means no extensions, not an unwrap"
        );
        assert_eq!(v.capability_source, SOURCE_NONE);
    }

    #[test]
    fn the_tasks_extension_is_read_from_the_extensions_map_by_its_spec_id() {
        let mut c = caps();
        let mut ext = rmcp::model::ExtensionCapabilities::new();
        ext.insert(TASKS_EXTENSION_ID.to_string(), Default::default());
        c.extensions = Some(ext);
        let v = ClientView::describe(None, None, Some(&c), SOURCE_INITIALIZE, vec![], false);
        assert!(
            v.declares_tasks,
            "declaring the SEP-2663 extension id must show as declares_tasks"
        );
        assert_eq!(v.extensions, vec![TASKS_EXTENSION_ID.to_string()]);
    }

    #[test]
    fn an_unrelated_extension_does_not_read_as_tasks() {
        let mut c = caps();
        let mut ext = rmcp::model::ExtensionCapabilities::new();
        ext.insert("com.example/something".to_string(), Default::default());
        c.extensions = Some(ext);
        let v = ClientView::describe(None, None, Some(&c), SOURCE_INITIALIZE, vec![], false);
        assert!(
            !v.declares_tasks,
            "any extension must not be mistaken for the tasks extension"
        );
        assert_eq!(v.extensions, vec!["com.example/something".to_string()]);
    }

    #[test]
    fn declaring_elicitation_and_having_no_usable_mode_are_reported_separately() {
        // The module's asymmetry (2), as an assertion: `client_capabilities()`
        // sees `_meta`, `supported_elicitation_modes()` does not. A view that
        // collapsed these into one boolean would claim this client can be
        // asked. It cannot.
        let mut c = caps();
        c.elicitation = Some(Default::default());
        let v = ClientView::describe(None, None, Some(&c), SOURCE_REQUEST_META, vec![], false);
        assert!(v.declares_elicitation);
        assert!(
            v.elicitation_modes.is_empty(),
            "the mode list is the gate elicit.rs actually uses and must not be \
             inferred from the declaration"
        );
    }

    #[test]
    fn the_env_gate_is_reported_independently_of_anything_the_client_said() {
        let mut c = caps();
        c.elicitation = Some(Default::default());
        let off = ClientView::describe(
            None,
            None,
            Some(&c),
            SOURCE_INITIALIZE,
            vec!["form".into()],
            false,
        );
        assert!(
            !off.asking_enabled,
            "a client declaring elicitation must never flip the server's own gate"
        );
        let on = ClientView::describe(None, None, None, SOURCE_NONE, vec![], true);
        assert!(
            on.asking_enabled,
            "and the gate must report on even when the client declared nothing"
        );
    }

    #[test]
    fn request_meta_wins_over_the_handshake_wherever_it_is_present() {
        // rmcp resolves `_meta` first, so its presence is the whole branch —
        // including the case where BOTH are present, which is the one a reader
        // would most likely get wrong.
        assert_eq!(source_of(true, true), SOURCE_REQUEST_META);
        assert_eq!(
            source_of(true, false),
            SOURCE_REQUEST_META,
            "meta-declared capabilities are, by construction, capabilities"
        );
    }

    #[test]
    fn without_meta_the_handshake_is_named_and_absence_is_named_separately() {
        assert_eq!(source_of(false, true), SOURCE_INITIALIZE);
        assert_eq!(
            source_of(false, false),
            SOURCE_NONE,
            "nothing visible must not be reported as a handshake declaration"
        );
    }

    #[test]
    fn extensions_are_sorted_so_the_report_is_stable_across_calls() {
        let mut c = caps();
        let mut ext = rmcp::model::ExtensionCapabilities::new();
        ext.insert("z.example/two".to_string(), Default::default());
        ext.insert("a.example/one".to_string(), Default::default());
        c.extensions = Some(ext);
        let v = ClientView::describe(None, None, Some(&c), SOURCE_INITIALIZE, vec![], false);
        assert_eq!(
            v.extensions,
            vec!["a.example/one".to_string(), "z.example/two".to_string()]
        );
    }

    #[test]
    fn the_client_name_and_version_survive_into_the_view() {
        // `#[non_exhaustive]` — constructor, not a struct literal.
        let impl_ = Implementation::new("claude-code", "2.9.9");
        let v = ClientView::describe(
            Some(impl_),
            Some(ProtocolVersion::LATEST),
            None,
            SOURCE_NONE,
            vec![],
            false,
        );
        assert_eq!(v.name.as_deref(), Some("claude-code"));
        assert_eq!(v.version.as_deref(), Some("2.9.9"));
        assert_eq!(
            v.protocol_version.as_deref(),
            Some(ProtocolVersion::LATEST.to_string().as_str())
        );
    }
}
