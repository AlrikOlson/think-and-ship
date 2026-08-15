//! Pure RFC 8628 (OAuth 2.0 Device Authorization Grant) core — the algorithmic
//! heart of `think-and-ship connect`.
//!
//! This module holds the wire response types, the §3.5 poll-decision logic
//! (when to keep waiting, when to back off, when to give up), and the
//! **transport-agnostic poll loop** that drives them. It is **network-free**:
//! the HTTP itself lives behind the [`DeviceTransport`] / [`Sleeper`] traits,
//! whose live reqwest + tokio implementations (plus the WorkOS token exchange
//! and the MCP-config write) live in the IO wire (`cli::connect`), which
//! consumes this core. Keeping the loop transport-agnostic makes it
//! exhaustively unit-testable with a scripted mock transport and a clock-free
//! sleeper — no server, no real time.

use std::time::Duration;

use serde::Deserialize;

/// RFC 8628 §3.2 default poll interval when the server omits `interval`.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// RFC 8628 §3.5: on `slow_down` the client MUST increase the interval by 5s.
const SLOW_DOWN_INCREMENT_SECS: u64 = 5;

/// The device-authorization response (RFC 8628 §3.2) — what the CLI gets back
/// from `POST {authorize_base}/device_authorization`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DeviceAuthResponse {
    /// The device verification code the CLI polls the token endpoint with.
    pub device_code: String,
    /// The short code the user types at `verification_uri`.
    pub user_code: String,
    /// The URL the user opens to approve.
    pub verification_uri: String,
    /// `verification_uri` with the `user_code` pre-filled (optional; §3.3.1).
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    /// Seconds until `device_code` expires.
    pub expires_in: u64,
    /// Minimum seconds between polls (optional; defaults to 5).
    #[serde(default)]
    pub interval: Option<u64>,
}

impl DeviceAuthResponse {
    /// The starting poll interval, applying the RFC 8628 default of 5s.
    #[must_use]
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.interval.unwrap_or(DEFAULT_POLL_INTERVAL_SECS))
    }
}

/// The body of a non-2xx token-poll response (RFC 8628 §3.5) — `{ "error": … }`.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenErrorBody {
    pub error: String,
}

/// A classified token-poll outcome. `Authorized` is set by the wire on a 2xx
/// (it holds the access token); the rest are classified from the `error` code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollStatus {
    /// The user approved — the wire now has the access token.
    Authorized,
    /// `authorization_pending` — the user hasn't approved yet; keep polling.
    Pending,
    /// `slow_down` — poll too fast; back off by +5s.
    SlowDown,
    /// `access_denied` — the user declined. Terminal.
    Denied,
    /// `expired_token` — the device code expired. Terminal.
    Expired,
    /// Any other error code. Terminal.
    Other(String),
}

impl PollStatus {
    /// Classify an RFC 8628 §3.5 token-error code into a poll status.
    #[must_use]
    pub fn from_error(error: &str) -> Self {
        match error {
            "authorization_pending" => Self::Pending,
            "slow_down" => Self::SlowDown,
            "access_denied" => Self::Denied,
            "expired_token" => Self::Expired,
            other => Self::Other(other.to_string()),
        }
    }
}

/// A terminal device-flow failure (the wire adds transport/HTTP variants).
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DeviceFlowError {
    #[error("the user denied the device authorization")]
    AccessDenied,
    #[error("the device code expired before approval")]
    Expired,
    #[error("device authorization failed: {0}")]
    Unexpected(String),
}

/// What the poll loop should do next, given a classified status + the current
/// interval. The wire executes this: succeed, sleep then re-poll, or abort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollDecision {
    /// Stop — the user approved.
    Authorized,
    /// Sleep this long, then poll again (carries the post-backoff interval).
    Wait(Duration),
    /// Stop — terminal failure.
    Fail(DeviceFlowError),
}

/// Pure RFC 8628 §3.5 poll decision: keep waiting on `authorization_pending`,
/// back off by +5s on `slow_down`, succeed on `Authorized`, and fail terminally
/// on denial / expiry / an unrecognized error.
#[must_use]
pub fn poll_decision(status: &PollStatus, interval: Duration) -> PollDecision {
    match status {
        PollStatus::Authorized => PollDecision::Authorized,
        PollStatus::Pending => PollDecision::Wait(interval),
        PollStatus::SlowDown => {
            PollDecision::Wait(interval + Duration::from_secs(SLOW_DOWN_INCREMENT_SECS))
        }
        PollStatus::Denied => PollDecision::Fail(DeviceFlowError::AccessDenied),
        PollStatus::Expired => PollDecision::Fail(DeviceFlowError::Expired),
        PollStatus::Other(code) => PollDecision::Fail(DeviceFlowError::Unexpected(code.clone())),
    }
}

/// One token-poll result, as classified by the transport: either the user
/// approved (carrying the WorkOS access token from the 2xx body) or a non-2xx
/// status code (RFC 8628 §3.5) for the loop to act on via [`poll_decision`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenPoll {
    /// The token endpoint returned 2xx — the user approved. Carries the token.
    Granted(String),
    /// The token endpoint returned a non-2xx `{ "error": … }` status to act on.
    Status(PollStatus),
}

/// The HTTP side of the device flow, abstracted so the poll loop is testable
/// without a live WorkOS server. The live reqwest implementation lives in
/// `cli::connect`; the tests below use a scripted in-memory transport.
///
/// `async fn` in a trait is fine here: the transport is always used through a
/// generic bound (never `dyn`) and driven on a current-thread runtime, so the
/// `Send` auto-trait that `async_fn_in_trait` warns about is irrelevant.
#[allow(async_fn_in_trait)]
pub trait DeviceTransport {
    /// `POST {authorize_base}/device_authorization` — start the flow.
    async fn device_authorize(&self) -> Result<DeviceAuthResponse, DeviceFlowError>;

    /// `POST {authorize_base}/token` once — classify the single poll outcome.
    async fn poll_token(&self, device_code: &str) -> Result<TokenPoll, DeviceFlowError>;
}

/// An async sleep, abstracted so the loop's backoff is testable without real
/// time. The live implementation wraps `tokio::time::sleep`; the test
/// implementation records the requested durations and returns immediately.
#[allow(async_fn_in_trait)] // generic, never `dyn`, current-thread runtime — see DeviceTransport.
pub trait Sleeper {
    /// Sleep for `dur`, then resolve.
    async fn sleep(&self, dur: Duration);
}

/// Drive the RFC 8628 §3.5 poll loop to a terminal outcome: poll the token
/// endpoint, apply [`poll_decision`] to each status, sleep the (possibly
/// backed-off) interval between polls, and return the WorkOS access token on
/// approval. The starting interval is the server's (defaulting to 5s); a
/// `slow_down` raises it by +5s, compounding.
///
/// A transport that classifies a poll as [`TokenPoll::Status`] yet yields the
/// degenerate [`PollStatus::Authorized`] (which carries no token) is treated as
/// a protocol error — the token only ever arrives via [`TokenPoll::Granted`].
pub async fn run_poll_loop<T: DeviceTransport, S: Sleeper>(
    transport: &T,
    sleeper: &S,
    auth: &DeviceAuthResponse,
) -> Result<String, DeviceFlowError> {
    let mut interval = auth.poll_interval();
    loop {
        match transport.poll_token(&auth.device_code).await? {
            TokenPoll::Granted(token) => return Ok(token),
            TokenPoll::Status(status) => match poll_decision(&status, interval) {
                PollDecision::Wait(next) => {
                    interval = next;
                    sleeper.sleep(next).await;
                }
                PollDecision::Fail(err) => return Err(err),
                PollDecision::Authorized => {
                    return Err(DeviceFlowError::Unexpected(
                        "transport classified a poll as a status but signalled \
                         authorization without a token"
                            .to_string(),
                    ));
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIVE: Duration = Duration::from_secs(5);

    #[test]
    fn classifies_the_rfc_8628_error_codes() {
        assert_eq!(
            PollStatus::from_error("authorization_pending"),
            PollStatus::Pending
        );
        assert_eq!(PollStatus::from_error("slow_down"), PollStatus::SlowDown);
        assert_eq!(PollStatus::from_error("access_denied"), PollStatus::Denied);
        assert_eq!(PollStatus::from_error("expired_token"), PollStatus::Expired);
        assert_eq!(
            PollStatus::from_error("invalid_grant"),
            PollStatus::Other("invalid_grant".into())
        );
    }

    #[test]
    fn pending_waits_the_current_interval() {
        assert_eq!(
            poll_decision(&PollStatus::Pending, FIVE),
            PollDecision::Wait(FIVE)
        );
    }

    #[test]
    fn slow_down_backs_off_by_five_seconds() {
        assert_eq!(
            poll_decision(&PollStatus::SlowDown, FIVE),
            PollDecision::Wait(Duration::from_secs(10))
        );
        // Backoff compounds when applied to an already-raised interval.
        assert_eq!(
            poll_decision(&PollStatus::SlowDown, Duration::from_secs(10)),
            PollDecision::Wait(Duration::from_secs(15))
        );
    }

    #[test]
    fn authorized_stops_polling() {
        assert_eq!(
            poll_decision(&PollStatus::Authorized, FIVE),
            PollDecision::Authorized
        );
    }

    #[test]
    fn denial_and_expiry_fail_terminally() {
        assert_eq!(
            poll_decision(&PollStatus::Denied, FIVE),
            PollDecision::Fail(DeviceFlowError::AccessDenied)
        );
        assert_eq!(
            poll_decision(&PollStatus::Expired, FIVE),
            PollDecision::Fail(DeviceFlowError::Expired)
        );
    }

    #[test]
    fn an_unrecognized_error_fails_with_the_code() {
        assert_eq!(
            poll_decision(&PollStatus::Other("invalid_client".into()), FIVE),
            PollDecision::Fail(DeviceFlowError::Unexpected("invalid_client".into()))
        );
    }

    #[test]
    fn deserializes_a_workos_device_authorization_response() {
        // Shape per RFC 8628 §3.2 / WorkOS CLI Auth.
        let json = r#"{
            "device_code": "dc_01HXYZ",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://auth.example.com/device",
            "verification_uri_complete": "https://auth.example.com/device?user_code=WDJB-MJHT",
            "expires_in": 1800,
            "interval": 5
        }"#;
        let resp: DeviceAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.user_code, "WDJB-MJHT");
        assert_eq!(
            resp.verification_uri_complete.as_deref(),
            Some("https://auth.example.com/device?user_code=WDJB-MJHT")
        );
        assert_eq!(resp.poll_interval(), FIVE);
    }

    #[test]
    fn applies_the_default_interval_when_the_server_omits_it() {
        let json = r#"{
            "device_code": "dc",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://auth.example.com/device",
            "expires_in": 600
        }"#;
        let resp: DeviceAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.interval, None);
        assert_eq!(resp.verification_uri_complete, None);
        assert_eq!(
            resp.poll_interval(),
            Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS)
        );
    }

    #[test]
    fn token_error_body_deserializes() {
        let body: TokenErrorBody = serde_json::from_str(r#"{"error":"slow_down"}"#).unwrap();
        assert_eq!(PollStatus::from_error(&body.error), PollStatus::SlowDown);
    }

    // --- run_poll_loop: the transport-agnostic loop, driven by a scripted
    // in-memory transport + a clock-free recording sleeper (no server, no time).

    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// A transport that replays a fixed script of poll outcomes in order.
    struct ScriptedTransport {
        polls: RefCell<VecDeque<Result<TokenPoll, DeviceFlowError>>>,
    }

    impl ScriptedTransport {
        fn new(polls: Vec<Result<TokenPoll, DeviceFlowError>>) -> Self {
            Self {
                polls: RefCell::new(polls.into()),
            }
        }
    }

    impl DeviceTransport for ScriptedTransport {
        async fn device_authorize(&self) -> Result<DeviceAuthResponse, DeviceFlowError> {
            Ok(auth_with_interval(Some(5)))
        }

        async fn poll_token(&self, _device_code: &str) -> Result<TokenPoll, DeviceFlowError> {
            self.polls
                .borrow_mut()
                .pop_front()
                .expect("scripted transport ran out of poll outcomes")
        }
    }

    /// A sleeper that records every requested duration and returns instantly.
    struct RecordingSleeper {
        slept: RefCell<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn new() -> Self {
            Self {
                slept: RefCell::new(Vec::new()),
            }
        }
    }

    impl Sleeper for RecordingSleeper {
        async fn sleep(&self, dur: Duration) {
            self.slept.borrow_mut().push(dur);
        }
    }

    fn auth_with_interval(interval: Option<u64>) -> DeviceAuthResponse {
        DeviceAuthResponse {
            device_code: "dc".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: "https://auth.example.com/device".into(),
            verification_uri_complete: None,
            expires_in: 600,
            interval,
        }
    }

    #[tokio::test]
    async fn loop_waits_through_pending_then_returns_the_granted_token() {
        let transport = ScriptedTransport::new(vec![
            Ok(TokenPoll::Status(PollStatus::Pending)),
            Ok(TokenPoll::Granted("wos_access_token".into())),
        ]);
        let sleeper = RecordingSleeper::new();
        let auth = auth_with_interval(Some(5));

        let token = run_poll_loop(&transport, &sleeper, &auth).await.unwrap();
        assert_eq!(token, "wos_access_token");
        // One pending poll → one sleep at the unchanged 5s interval.
        assert_eq!(*sleeper.slept.borrow(), vec![FIVE]);
    }

    #[tokio::test]
    async fn loop_backs_off_on_slow_down_and_keeps_the_raised_interval() {
        let transport = ScriptedTransport::new(vec![
            Ok(TokenPoll::Status(PollStatus::SlowDown)),
            Ok(TokenPoll::Status(PollStatus::Pending)),
            Ok(TokenPoll::Granted("t".into())),
        ]);
        let sleeper = RecordingSleeper::new();
        let auth = auth_with_interval(Some(5));

        let token = run_poll_loop(&transport, &sleeper, &auth).await.unwrap();
        assert_eq!(token, "t");
        // slow_down raises 5s→10s; the subsequent pending waits the RAISED 10s.
        assert_eq!(
            *sleeper.slept.borrow(),
            vec![Duration::from_secs(10), Duration::from_secs(10)]
        );
    }

    #[tokio::test]
    async fn loop_fails_terminally_on_denial_without_sleeping() {
        let transport = ScriptedTransport::new(vec![Ok(TokenPoll::Status(PollStatus::Denied))]);
        let sleeper = RecordingSleeper::new();
        let auth = auth_with_interval(Some(5));

        let err = run_poll_loop(&transport, &sleeper, &auth)
            .await
            .unwrap_err();
        assert_eq!(err, DeviceFlowError::AccessDenied);
        assert!(sleeper.slept.borrow().is_empty());
    }

    #[tokio::test]
    async fn loop_fails_terminally_on_expiry() {
        let transport = ScriptedTransport::new(vec![
            Ok(TokenPoll::Status(PollStatus::Pending)),
            Ok(TokenPoll::Status(PollStatus::Expired)),
        ]);
        let sleeper = RecordingSleeper::new();
        let auth = auth_with_interval(Some(5));

        let err = run_poll_loop(&transport, &sleeper, &auth)
            .await
            .unwrap_err();
        assert_eq!(err, DeviceFlowError::Expired);
    }

    #[tokio::test]
    async fn loop_treats_a_status_authorized_as_a_protocol_error() {
        // The token only ever arrives via Granted; a Status(Authorized) is degenerate.
        let transport = ScriptedTransport::new(vec![Ok(TokenPoll::Status(PollStatus::Authorized))]);
        let sleeper = RecordingSleeper::new();
        let auth = auth_with_interval(Some(5));

        let err = run_poll_loop(&transport, &sleeper, &auth)
            .await
            .unwrap_err();
        assert!(matches!(err, DeviceFlowError::Unexpected(_)));
    }

    #[tokio::test]
    async fn device_authorize_is_reachable_through_the_trait() {
        // Exercises the scripted authorize path so the trait method isn't dead.
        let transport = ScriptedTransport::new(vec![]);
        let auth = transport.device_authorize().await.unwrap();
        assert_eq!(auth.poll_interval(), FIVE);
    }
}
