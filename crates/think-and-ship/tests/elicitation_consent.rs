//! End-to-end proof that the server can ask a human — and cannot ask anyone
//! else (roadmap chunk `mcp-elicitation-consent`).
//!
//! Every test here drives a REAL rmcp client over an in-memory duplex, the same
//! harness as `progress_notifications.rs`. Nothing inspects our internals: each
//! assertion is either about what crossed the wire, or about the value our seam
//! returned after a real client answered it.
//!
//! # Why the peer is taken from the server side
//!
//! Elicitation runs server→client, so the thing under test is
//! `Peer<RoleServer>` — which exists only inside a live session. Each test
//! therefore keeps the server's `RunningService` and calls
//! [`think_and_ship::mcp::elicit::ask_propose_consent`] with its real peer. The
//! `elicitation/create` request genuinely goes out over the duplex and the
//! client's genuine `ElicitResult` comes back.
//!
//! # The one thing these tests deliberately do NOT drive
//!
//! The production trigger is a successful `tracker_setup`, which cannot succeed
//! in a test: `build_tracker_port` knows only `github` and `linear`, both of
//! which need real credentials, and `linear` reaches the network and comes back
//! 401 even with `dry_run`. That gap is already filed as
//! `tracker-port-test-seam`. Here the gate is left at its production default
//! (unset); `elicitation_tool_seam.rs` is the gate-ON half, and the
//! reachability chain is spelled out in its module doc.
//!
//! # The claims, one test each
//!
//! 1. a capable client that accepts yields the human's answer,
//! 2. a client that DECLINES collapses to no answer,
//! 3. a client that CANCELS collapses identically,
//! 4. a client that never answers TIMES OUT — bounded, and it returns,
//! 5. a client that never declared elicitation is never even asked,
//! 6. **the gate off means no question crosses the wire, even to a client that
//!    declares the capability and would happily accept**,
//! 7. an already-answered consent is not asked again.
//!
//! An eighth claim was removed rather than fixed; see the comment where it used
//! to live, near the bottom of this file.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{ClientCapabilities, ClientInfo, ElicitRequestParams, ElicitResult, ElicitationAction},
    service::RequestContext,
};
use serde_json::json;
use think_and_ship::mcp::UnifiedService;
use think_and_ship::mcp::elicit::{AskOutcome, Unanswered, ask_propose_consent};
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;
use think_and_ship::tracker::propose_consent::{ProposeConsent, ProposeConsentSource};

/// What a test client does when the server asks it something.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Accept(bool),
    Decline,
    Cancel,
    /// Never respond. Used to prove the wait is bounded.
    Stall,
    /// Accept with a body that is not the requested shape.
    Garbage,
}

/// A client that records every elicitation it was asked and answers as told.
///
/// `declares` is separate from `answers` on purpose: the whole point of claim 6
/// is that a client which declares the capability AND would accept still gets
/// asked nothing when the server-side gate is off.
#[derive(Clone)]
struct AskableClient {
    declares: bool,
    answers: Answer,
    asked: Arc<Mutex<Vec<String>>>,
}

impl AskableClient {
    fn new(declares: bool, answers: Answer) -> Self {
        Self {
            declares,
            answers,
            asked: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ClientHandler for AskableClient {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        if self.declares {
            info.capabilities = ClientCapabilities::builder().enable_elicitation().build();
        }
        info
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, rmcp::ErrorData> {
        let message = match &request {
            ElicitRequestParams::FormElicitationParams { message, .. } => message.clone(),
            ElicitRequestParams::UrlElicitationParams { message, .. } => message.clone(),
            _ => String::new(),
        };
        self.asked.lock().unwrap().push(message);
        match self.answers {
            Answer::Accept(enabled) => {
                let mut result = ElicitResult::new(ElicitationAction::Accept);
                result.content = Some(json!({ "enabled": enabled }));
                Ok(result)
            }
            Answer::Decline => Ok(ElicitResult::new(ElicitationAction::Decline)),
            Answer::Cancel => Ok(ElicitResult::new(ElicitationAction::Cancel)),
            Answer::Garbage => {
                let mut result = ElicitResult::new(ElicitationAction::Accept);
                result.content = Some(json!({ "not_the_field": "at all" }));
                Ok(result)
            }
            Answer::Stall => {
                // Longer than any wait a test passes in, so the server's bound
                // is the only thing that can end this.
                tokio::time::sleep(Duration::from_secs(600)).await;
                Ok(ElicitResult::new(ElicitationAction::Decline))
            }
        }
    }
}

fn build_unified(project: &str) -> UnifiedService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(cfg));
    let ship = ShipService::new(ShipEngine::new(project.into()));
    let roadmap = RoadmapService::new(RoadmapEngine::new(project.into()));
    let signal = SignalService::new(SignalEngine::new(project.into()));
    UnifiedService::new(think, ship, roadmap, signal)
}

struct Session {
    client: rmcp::service::RunningService<RoleClient, AskableClient>,
    /// The SERVER's peer — the only handle that can ask a client anything.
    peer: rmcp::Peer<rmcp::RoleServer>,
    /// Every elicitation message the client was actually sent.
    asked: Arc<Mutex<Vec<String>>>,
}

/// Both halves must be started concurrently: `serve` completes the initialize
/// handshake, so awaiting the server's before the client exists deadlocks.
async fn session(project: &str, client: AskableClient) -> Session {
    let server = build_unified(project);
    let (server_tx, client_tx) = tokio::io::duplex(4096);

    let (peer_tx, peer_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let running = server.serve(server_tx).await.expect("server.serve failed");
        let _ = peer_tx.send(running.peer().clone());
        let _ = running.waiting().await;
    });

    let asked = client.asked.clone();
    let client = client.serve(client_tx).await.expect("client.serve failed");
    let peer = peer_rx.await.expect("server peer");
    Session {
        client,
        peer,
        asked,
    }
}

fn undecided() -> ProposeConsent {
    ProposeConsent::default()
}

/// A short bound so the timeout claim does not drag the suite. The production
/// value is `elicit::ASK_TIMEOUT`; what is under test is that a bound exists
/// and is honoured, not its magnitude.
const TEST_WAIT: Duration = Duration::from_secs(2);

/// Claim 1: a real human, a real answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_accepts_yields_the_humans_answer() {
    let s = session(
        "elicit-accept",
        AskableClient::new(true, Answer::Accept(true)),
    )
    .await;

    let outcome = ask_propose_consent(&s.peer, true, &undecided(), TEST_WAIT).await;

    assert_eq!(outcome, AskOutcome::Answered(true));
    assert_eq!(outcome.answer(), Some(true));
    assert_eq!(
        s.asked.lock().unwrap().len(),
        1,
        "exactly one question should have crossed the wire"
    );
    assert!(
        s.asked.lock().unwrap()[0].contains("PROPOSALS"),
        "the human must be told what they are consenting to: {:?}",
        s.asked.lock().unwrap()
    );

    // The twin: a human who says NO is an answer too, not an absence.
    let s2 = session(
        "elicit-accept-no",
        AskableClient::new(true, Answer::Accept(false)),
    )
    .await;
    let outcome2 = ask_propose_consent(&s2.peer, true, &undecided(), TEST_WAIT).await;
    assert_eq!(outcome2.answer(), Some(false));

    let _ = s.client.cancel().await;
    let _ = s2.client.cancel().await;
}

/// Claim 2: declined is not an error and not a yes — it is the default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_declines_collapses_to_no_answer() {
    let s = session("elicit-decline", AskableClient::new(true, Answer::Decline)).await;

    let outcome = ask_propose_consent(&s.peer, true, &undecided(), TEST_WAIT).await;

    assert_eq!(outcome, AskOutcome::Unanswered(Unanswered::Declined));
    assert_eq!(
        outcome.answer(),
        None,
        "a decline must be indistinguishable from every other non-answer"
    );
    assert_eq!(s.asked.lock().unwrap().len(), 1, "the ask did happen");

    let _ = s.client.cancel().await;
}

/// Claim 3: dismissed, and a body that is not an answer, land in the same
/// place. Two distinct client behaviours, one observable result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_or_unusable_answer_collapses_identically() {
    let cancelled = session("elicit-cancel", AskableClient::new(true, Answer::Cancel)).await;
    let out_cancel = ask_propose_consent(&cancelled.peer, true, &undecided(), TEST_WAIT).await;
    assert_eq!(out_cancel, AskOutcome::Unanswered(Unanswered::Cancelled));
    assert_eq!(out_cancel.answer(), None);

    let garbled = session("elicit-garbage", AskableClient::new(true, Answer::Garbage)).await;
    let out_garbage = ask_propose_consent(&garbled.peer, true, &undecided(), TEST_WAIT).await;
    assert_eq!(out_garbage, AskOutcome::Unanswered(Unanswered::Unusable));
    assert_eq!(
        out_garbage.answer(),
        None,
        "an answer we cannot read must not be guessed at"
    );

    let _ = cancelled.client.cancel().await;
    let _ = garbled.client.cancel().await;
}

/// Claim 4: THE ONE THE USER'S RULE IS ABOUT. A client that declares the
/// capability and then never answers must not hold the call open.
///
/// This is what makes the bound real rather than nominal. `ask_propose_consent`
/// takes a `Duration`, not an `Option<Duration>`, precisely so that no caller
/// can produce the unbounded form — rmcp's `elicit_with_timeout(msg, None)` IS
/// `elicit()`, so the method name guarantees nothing on its own. Restore the
/// `Option` and pass `None`, and this test never finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_never_answers_times_out_and_the_call_returns() {
    let s = session("elicit-stall", AskableClient::new(true, Answer::Stall)).await;

    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        TEST_WAIT * 6,
        ask_propose_consent(&s.peer, true, &undecided(), TEST_WAIT),
    )
    .await
    .expect("the ask MUST return on its own; an unbounded elicitation hangs here forever");

    assert_eq!(outcome, AskOutcome::Unanswered(Unanswered::TimedOut));
    assert_eq!(outcome.answer(), None);
    assert!(
        started.elapsed() < TEST_WAIT * 4,
        "the wait must be bounded by the duration passed in, took {:?}",
        started.elapsed()
    );

    let _ = s.client.cancel().await;
}

/// Claim 5: degrade, never fail. A client with no elicitation
/// capability is never asked, and gets the default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_declared_nothing_is_never_asked() {
    // `answers` says Accept, so if a question DID reach it the outcome would be
    // Answered(true) — the test cannot pass by the client simply being unhelpful.
    let s = session(
        "elicit-nocap",
        AskableClient::new(false, Answer::Accept(true)),
    )
    .await;

    let outcome = ask_propose_consent(&s.peer, true, &undecided(), TEST_WAIT).await;

    assert_eq!(outcome, AskOutcome::Unanswered(Unanswered::ClientCannotAsk));
    assert_eq!(outcome.answer(), None);
    assert!(
        s.asked.lock().unwrap().is_empty(),
        "nothing may be put to a client that never said it could answer"
    );

    let _ = s.client.cancel().await;
}

/// Claim 6: **the load-bearing one.** The client declares elicitation and would
/// accept. The server-side gate is off. Nothing may cross the wire.
///
/// Claude Code has declared this capability since v2.1.76, so it declares it in
/// headless `claude -p`, in cron and in subagents. If the capability
/// declaration could turn asking on, every unattended run would be one
/// `tracker_setup` away from a prompt nobody will ever see. That is the failure
/// this test exists to make impossible.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gate_being_off_beats_a_client_that_declares_and_would_accept() {
    let s = session(
        "elicit-gate-off",
        AskableClient::new(true, Answer::Accept(true)),
    )
    .await;

    let outcome = ask_propose_consent(
        &s.peer,
        false, // the env gate: default OFF
        &undecided(),
        TEST_WAIT,
    )
    .await;

    assert_eq!(outcome, AskOutcome::Unanswered(Unanswered::AskingDisabled));
    assert_eq!(outcome.answer(), None);
    assert!(
        s.asked.lock().unwrap().is_empty(),
        "the gate must be checked BEFORE the peer is touched; a question reached \
         the client anyway: {:?}",
        s.asked.lock().unwrap()
    );

    let _ = s.client.cancel().await;
}

/// Claim 7: asked once, never again — in either direction. A remembered NO is a
/// decision, not an absence, so it must silence the question just as a YES does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_already_answered_consent_is_not_asked_again() {
    for remembered_answer in [true, false] {
        let s = session(
            "elicit-decided",
            AskableClient::new(true, Answer::Accept(true)),
        )
        .await;
        let decided = ProposeConsent {
            enabled: remembered_answer,
            decided_at: Some("2026-07-27T10:00:00Z".into()),
            source: ProposeConsentSource::Human,
        };

        let outcome = ask_propose_consent(&s.peer, true, &decided, TEST_WAIT).await;

        assert_eq!(outcome, AskOutcome::Unanswered(Unanswered::AlreadyDecided));
        assert!(
            s.asked.lock().unwrap().is_empty(),
            "a human who already answered {remembered_answer} must not be asked again"
        );
        let _ = s.client.cancel().await;
    }
}

// CLAIM 8 IS DELIBERATELY ABSENT, and its absence is a finding rather than an
// omission.
//
// It read "a tracker_setup that failed asks nobody anything" and drove a real
// `tools/call` with a capable, accepting client. Sabotaging the env gate turned
// it red — which is the wrong test failing, and proved it had been measuring
// the gate all along, never the landed check it named. Fixing `landed` (it was
// reading `is_error`, which this server sets to `false` on failures ON PURPOSE)
// then made it unfailable from either direction: gate unset OR call not landed,
// each alone is enough for silence.
//
// A test that cannot fail for its stated reason is worse than no test, because
// it reads like coverage. The claim moved to `elicitation_tool_seam.rs`, which
// runs in its own process with the gate turned ON so that the landed check is
// the only thing left holding the silence.

/// The moment, by exact name rather than by "some tool asks".
///
/// A predicate that returned true for everything would leave every claim above
/// green while turning the whole surface into a nag machine.
#[test]
fn exactly_one_tool_is_the_moment_to_ask() {
    assert!(RoadmapService::consent_question_for("tracker_setup"));
    for other in [
        "tracker_status",
        "roadmap_status",
        "roadmap_add_chunk",
        "think_record_step",
        "ship_check",
        "",
    ] {
        assert!(
            !RoadmapService::consent_question_for(other),
            "{other} must not trigger a consent prompt"
        );
    }
}

/// The call site is REACHED, not merely present.
///
/// A source-text gate, because the positive tool-level path cannot be executed
/// without a test seam for the tracker port (`build_tracker_port` knows only
/// github and linear, both of which need a network). Matched WITH its
/// indentation so a commented-out or differently-scoped call cannot satisfy it
/// — the failure mode that let `propose_status_from_sweep` ship dead,
/// undetected, for a long stretch of development.
#[test]
fn the_roadmap_seam_actually_calls_the_consent_ask() {
    let src = include_str!("../src/roadmap/mcp/service.rs");
    assert!(
        src.contains(
            "\n            let _ = crate::mcp::elicit::ask_and_remember_propose_consent(&peer, \
             &data_dir).await;"
        ),
        "RoadmapService::call_tool no longer asks; the elicitation point is dead \
         in production, which is the exact state this gate exists to prevent"
    );
    assert!(
        src.contains("if asked_about && Self::landed(&result) {"),
        "the ask must stay gated on the tool being the moment AND the call \
         having landed"
    );
}
