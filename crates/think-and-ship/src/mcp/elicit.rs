//! Asking the human, mid-tool-call.
//!
//! This module is the **only** place in the crate that calls MCP elicitation.
//! That is not tidiness; it is the enforcement mechanism for the rule below.
//!
//! # The rule, and why the obvious guard is not enough
//!
//! Elicitation must never be able to interrupt or hang an autonomous session
//! with no human present. The tempting guard is "only ask clients that declared
//! the capability" — and it is **insufficient**. Claude Code shipped MCP
//! elicitation in v2.1.76 (2026-03-14), so it declares the capability in
//! headless `claude -p`, in cron runs and in subagents, where the prompt will
//! be answered by nobody and seen by nobody.
//!
//! This is the same trap [`crate::mcp::progress`] documents: rmcp's client
//! sets a `progressToken` on *every* request unconditionally, so a token's
//! presence was never evidence of consent. Here, a capability **declaration is
//! not a human being present**.
//!
//! # The four parts, all required
//!
//! 1. **A default-OFF env gate**, [`ASK_ENV`], checked *first* — before the peer
//!    is touched at all. Nothing about the client can turn asking on; only a
//!    deliberate opt-in on this machine can.
//! 2. **A capability check** via `supported_elicitation_modes()`, which returns
//!    an empty set when the client never declared elicitation.
//! 3. **A bounded wait, structurally.** [`ask_propose_consent`] takes a
//!    `Duration`, not an `Option<Duration>`. rmcp's `elicit_with_timeout` accepts
//!    `None` and `elicit()` is literally `elicit_with_timeout(msg, None)` — so
//!    calling the `_with_timeout` *name* guarantees nothing. Making the
//!    parameter non-optional means the unbounded call cannot be expressed
//!    through this seam at all.
//! 4. **One collapse path.** Declined, cancelled, timed out, unsupported,
//!    unparseable, empty, asking-disabled and already-decided are eight distinct
//!    situations and exactly one outcome: [`AskOutcome::answer`] returns `None`,
//!    nothing is written, nothing errors, and the tool call returns the same
//!    bytes it would have returned if this module did not exist.
//!
//! # What is being asked
//!
//! One question, at one moment: after a human has run `tracker_setup`, may the
//! unattended sweep record proposals on their roadmap? Today that is
//! `THINK_AND_SHIP_TRACKER_PROPOSE`, default-off — an env var so invisible that
//! dedicated follow-up work was needed just to make it announce itself. The
//! answer is remembered in [`crate::tracker::propose_consent`], so nobody is
//! asked twice.
//!
//! The choice of *this* question over the two alternatives considered is
//! forced by part 4. A destructive confirmation before `think_wipe_trace` has no
//! working collapse: with no human there, it must either wipe unconfirmed (the
//! guard buys nothing) or refuse (the collapse becomes a failure path). Asking
//! `tracker_setup` which team to mirror into collapses to "error, pass `--into`"
//! — a failure path again. Only the propose switch has a collapse target that
//! already exists, already works, and is already the safe choice: stay off.

use std::time::Duration;

use rmcp::{
    Peer, RoleServer,
    service::{ElicitationError, ElicitationMode, ServiceError},
};
use serde::Deserialize;

use crate::tracker::propose_consent::ProposeConsent;

/// Opt-in for the server to ask the human anything at all. Default OFF.
///
/// Deliberately a *separate* switch from what is being consented to. The
/// remembered answer lives on disk; this says whether a question may be put on
/// the wire in the first place. An unattended harness sets neither.
pub const ASK_ENV: &str = "THINK_AND_SHIP_ELICIT";

/// How long a question may wait for an answer before the call gives up and
/// takes the default. Short enough that a forgotten prompt never holds a tool
/// call for a meaningful fraction of a session.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(60);

/// Whether the server may ask, from the raw env value.
///
/// Only an explicit `1` or `true` enables it — the writer's fallback direction
/// this codebase already uses for `THINK_AND_SHIP_TRACKER_PROPOSE`: a typo
/// means off, because an unexpected prompt in an unattended run is worse than a
/// question that never gets asked.
#[must_use]
pub fn asking_enabled_from(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|r| r.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true")
    )
}

/// The env read, split off so everything above it is testable.
#[must_use]
pub fn asking_enabled() -> bool {
    asking_enabled_from(std::env::var(ASK_ENV).ok().as_deref())
}

/// The exact question a human sees. A pure function so the wording is
/// assertable by value rather than by "a message was present".
#[must_use]
pub fn propose_consent_prompt() -> String {
    "think-and-ship can watch your issue tracker in the background and record \
     what changed as PROPOSALS on your roadmap — suggested status and title \
     changes that you accept or reject. It never edits the plan on its own. \
     May it do that? (You can change this any time with \
     THINK_AND_SHIP_TRACKER_PROPOSE.)"
        .to_string()
}

/// The shape a client fills in. One boolean, no free text: the whole decision.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ProposeConsentAnswer {
    /// Whether background sweeps may record proposals on the roadmap.
    pub enabled: bool,
}

rmcp::elicit_safe!(ProposeConsentAnswer);

/// Why no answer was obtained. Carried for the operator-facing log line only —
/// every variant leads to the identical behaviour, which is what
/// [`AskOutcome::answer`] enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unanswered {
    /// [`ASK_ENV`] is not set. The unattended default, and the only guard that
    /// nothing about the client can override.
    AskingDisabled,
    /// A human already answered once; asking again would be nagging.
    AlreadyDecided,
    /// The client declared no elicitation capability.
    ClientCannotAsk,
    /// The human said no to answering.
    Declined,
    /// The human dismissed the prompt.
    Cancelled,
    /// Nobody answered inside the bound.
    TimedOut,
    /// The client answered with something that is not an answer (unparseable,
    /// empty, or a transport failure).
    Unusable,
}

/// The result of putting the question. Constructed only by
/// [`ask_propose_consent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskOutcome {
    /// A human answered, and this is what they said.
    Answered(bool),
    /// No answer, for one of eight reasons that all mean the same thing.
    Unanswered(Unanswered),
}

impl AskOutcome {
    /// The **only** way a caller may read this outcome.
    ///
    /// This is part 4 of the rule expressed as a type: every non-answer is
    /// `None`, so no caller can accidentally treat "timed out" differently from
    /// "declined" differently from "the client cannot be asked". A caller that
    /// matched on [`Unanswered`] could invent a divergence; this signature means
    /// there is nothing to diverge on.
    #[must_use]
    pub fn answer(self) -> Option<bool> {
        match self {
            Self::Answered(v) => Some(v),
            Self::Unanswered(_) => None,
        }
    }
}

/// Ask whether unattended sweeps may propose — or, far more often, decline to
/// ask at all.
///
/// `asking_enabled` and `remembered` are parameters rather than reads so the
/// DECISION is reachable by a test while only the env/disk access stays
/// untestable — the same split `unattended_propose_enabled` uses.
///
/// `wait` is a `Duration` and not an `Option<Duration>` **on purpose**: see
/// part 3 of the module rule. The unbounded elicitation call is not reachable
/// from anywhere in this crate because it cannot be spelled here.
pub async fn ask_propose_consent(
    peer: &Peer<RoleServer>,
    asking_enabled: bool,
    remembered: &ProposeConsent,
    wait: Duration,
) -> AskOutcome {
    // Part 1. FIRST, and before the peer is touched: no property of the client
    // can reach past this line.
    if !asking_enabled {
        return AskOutcome::Unanswered(Unanswered::AskingDisabled);
    }
    // Ask at most once, in either direction. A remembered "no" is a decision,
    // not an absence.
    if remembered.is_decided() {
        return AskOutcome::Unanswered(Unanswered::AlreadyDecided);
    }
    // Part 2.
    if !peer
        .supported_elicitation_modes()
        .contains(&ElicitationMode::Form)
    {
        tracing::warn!(
            "[think-and-ship] {ASK_ENV} is on, but this client declared no \
             elicitation capability — keeping the default (proposals off)."
        );
        return AskOutcome::Unanswered(Unanswered::ClientCannotAsk);
    }
    eprintln!("[think-and-ship] asking the human whether background sweeps may propose.");
    // Part 3. `wait` is never `None`, because it cannot be.
    match peer
        .elicit_with_timeout::<ProposeConsentAnswer>(propose_consent_prompt(), Some(wait))
        .await
    {
        Ok(Some(answer)) => AskOutcome::Answered(answer.enabled),
        // Part 4: from here down, every arm is the same behaviour.
        Ok(None) | Err(ElicitationError::NoContent) => AskOutcome::Unanswered(Unanswered::Unusable),
        Err(ElicitationError::UserDeclined) => AskOutcome::Unanswered(Unanswered::Declined),
        Err(ElicitationError::UserCancelled) => AskOutcome::Unanswered(Unanswered::Cancelled),
        Err(ElicitationError::Service(ServiceError::Timeout { .. })) => {
            AskOutcome::Unanswered(Unanswered::TimedOut)
        }
        Err(_) => AskOutcome::Unanswered(Unanswered::Unusable),
    }
}

/// The production wrapper: read the two gates, ask, and remember an answer.
///
/// Returns the outcome for logging. Nothing here can fail the caller — a
/// persistence error is swallowed for the same reason a failed progress
/// notification is: the tool call is unaffected, and reporting it would turn a
/// courtesy into an error path.
pub async fn ask_and_remember_propose_consent(
    peer: &Peer<RoleServer>,
    data_dir: &std::path::Path,
) -> AskOutcome {
    let remembered = crate::tracker::propose_consent::load(data_dir);
    let outcome = ask_propose_consent(peer, asking_enabled(), &remembered, ASK_TIMEOUT).await;
    if let Some(enabled) = outcome.answer() {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = crate::tracker::propose_consent::record(data_dir, enabled, &now);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_is_off_unless_explicitly_turned_on() {
        assert!(!asking_enabled_from(None), "unset must mean no asking");
        assert!(!asking_enabled_from(Some("0")));
        assert!(!asking_enabled_from(Some("false")));
        assert!(
            !asking_enabled_from(Some("yes")),
            "a typo must not enable prompting in an unattended run"
        );
        assert!(asking_enabled_from(Some("1")));
        assert!(asking_enabled_from(Some(" TRUE ")));
    }

    #[test]
    fn every_non_answer_collapses_to_the_same_none() {
        // Part 4 as an exhaustive assertion. If a ninth reason is added and it
        // is not an answer, it must land here too.
        for reason in [
            Unanswered::AskingDisabled,
            Unanswered::AlreadyDecided,
            Unanswered::ClientCannotAsk,
            Unanswered::Declined,
            Unanswered::Cancelled,
            Unanswered::TimedOut,
            Unanswered::Unusable,
        ] {
            assert_eq!(
                AskOutcome::Unanswered(reason).answer(),
                None,
                "{reason:?} must be indistinguishable from every other non-answer"
            );
        }
        assert_eq!(AskOutcome::Answered(true).answer(), Some(true));
        assert_eq!(AskOutcome::Answered(false).answer(), Some(false));
    }

    #[test]
    fn the_prompt_says_what_is_being_consented_to_and_how_to_change_it() {
        let p = propose_consent_prompt();
        assert!(
            p.contains("PROPOSALS"),
            "the human must be told the sweep proposes rather than edits"
        );
        assert!(
            p.contains("never edits the plan on its own"),
            "the limit of the consent is the load-bearing sentence"
        );
        assert!(
            p.contains("THINK_AND_SHIP_TRACKER_PROPOSE"),
            "a consent with no stated way to revoke it is not consent"
        );
    }
}
