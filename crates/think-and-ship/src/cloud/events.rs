//! Realtime push-receive: subscribe to the backend's
//! `/v1/events` WebSocket and refresh the local cache when the tenant's
//! records change, so a local server learns about remote mutations live
//! instead of only at startup.
//!
//! Shape mirrors the backend half that already shipped: the tenant DO
//! broadcasts a small [`RecordNotification`] after each successful write
//! (`backend/src/realtime.ts`). This client consumes it with bounded
//! reconnect backoff: on a notification, re-pull the changed family through the existing
//! `cloud::pull` machinery (silent cloud-wins upserts — a refresh never loops
//! back into a push).
//!
//! Testability follows `cloud::device_flow`: the socket and the refresh sit
//! behind traits ([`EventsTransport`] / [`EventsStream`] / [`FamilyRefresher`]),
//! time behind the shared [`Sleeper`], and the reconnect/fallback policy is a
//! pure function ([`after_failure`]) — the loop unit-tests with scripted mocks
//! and zero sockets. The live tungstenite transport never returns
//! [`EventsError::Shutdown`]; only test scripts do, to end the loop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::cloud::client::{CloudClient, CloudError};
use crate::cloud::device_flow::Sleeper;
use crate::cloud::pull;
use crate::roadmap::RoadmapEngine;
use crate::signal::SignalEngine;
use crate::think::engine::core::ReasoningServer;

/// A live record-change push from the tenant DO — the Rust mirror of
/// `RecordNotification` in `backend/src/realtime.ts`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RecordNotification {
    #[serde(rename = "type")]
    pub kind_tag: String,
    pub family: String,
    /// The changed envelope itself (ws-delta-push) — present from newer
    /// servers; `None` falls back to the family refresh.
    #[serde(default)]
    pub envelope: Option<serde_json::Value>,
}

/// Parse one WS text frame into a record-change notification. Anything that
/// isn't JSON with `type == "record.created"` is `None` (ignored) — exactly
/// the SPA's `parseRecordNotification` contract.
pub fn parse_notification(text: &str) -> Option<RecordNotification> {
    let note: RecordNotification = serde_json::from_str(text).ok()?;
    (note.kind_tag == "record.created").then_some(note)
}

/// A doorbell frame: a tracker had news.
///
/// Note what it does NOT carry — any issue identity. The edge is not allowed to
/// say WHICH item moved, because a subscriber that acted on that would be acting
/// on link state the edge has no way to reason about. The only instruction is
/// "look again", and the looking is [`TrackerSweeper`]'s job.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TrackerNews {
    #[serde(rename = "type")]
    pub kind_tag: String,
    pub provider: String,
}

/// Parse one WS text frame as a tracker doorbell, or `None`.
///
/// Kept separate from [`parse_notification`] rather than folded into one enum:
/// the two frames mean genuinely different things (a row exists / go look at
/// somebody else's system), and an older build that knows only the first must
/// keep ignoring this one, which it does for free.
pub fn parse_tracker_news(text: &str) -> Option<TrackerNews> {
    let note: TrackerNews = serde_json::from_str(text).ok()?;
    (note.kind_tag == "tracker.news").then_some(note)
}

/// Consecutive connect failures after which each retry cycle also does a full
/// poll refresh, so the cache keeps converging while the WS is unavailable.
pub const FALLBACK_AFTER_FAILURES: u32 = 3;

/// How long a fallback (polling) cycle sleeps between attempts.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Bounded exponential reconnect backoff, mirroring the SPA: 2s, 4s, 8s, …
/// capped at 30s. `attempt` is the consecutive-failure count (≥ 1).
pub fn backoff_delay(attempt: u32) -> Duration {
    let secs = 1u64 << attempt.clamp(1, 6).min(5);
    Duration::from_secs(secs.min(30))
}

/// What a cycle does after `consecutive_failures` (≥ 1): plain backoff sleep,
/// or — once the WS looks unavailable — a poll refresh plus the poll interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleAction {
    /// Sleep, then retry the WS connect.
    Sleep(Duration),
    /// Refresh all families by polling, sleep, then retry the WS connect.
    PollAndSleep(Duration),
}

/// The pure reconnect/fallback policy.
pub fn after_failure(consecutive_failures: u32) -> CycleAction {
    if consecutive_failures >= FALLBACK_AFTER_FAILURES {
        CycleAction::PollAndSleep(POLL_INTERVAL)
    } else {
        CycleAction::Sleep(backoff_delay(consecutive_failures))
    }
}

/// How often the live stream pings a quiet connection (proof-of-life probe).
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Total silence (no frames, no pongs) after which the connection is declared
/// dead. A zombie socket — the edge keeps TCP ESTABLISHED while the DO behind
/// it was replaced — never errors and never closes, so without
/// this limit the subscriber would pend on the read forever.
pub const IDLE_GIVE_UP: Duration = Duration::from_secs(90);

/// What the live stream should do given how long the connection has been
/// silent and how long since the last ping. Pure — the IO glue in
/// [`WsStream`] supplies real elapsed times; tests supply synthetic ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepaliveAction {
    /// Keep reading, but for at most this long before re-consulting.
    Wait(Duration),
    /// The connection has been quiet long enough to probe — send a ping.
    SendPing,
    /// Total silence past [`IDLE_GIVE_UP`] — treat the stream as dropped so
    /// the reconnect + full-refresh machinery engages.
    GiveUp,
}

/// The pure keepalive policy. A pong (or any frame) counts as life, so a
/// healthy idle connection cycles ping→pong and never gives up; a zombie gets
/// no pong and is declared dead within [`IDLE_GIVE_UP`].
pub fn keepalive_action(since_last_rx: Duration, since_last_ping: Duration) -> KeepaliveAction {
    if since_last_rx >= IDLE_GIVE_UP {
        return KeepaliveAction::GiveUp;
    }
    if since_last_ping >= PING_INTERVAL {
        return KeepaliveAction::SendPing;
    }
    let until_ping = PING_INTERVAL - since_last_ping;
    let until_give_up = IDLE_GIVE_UP - since_last_rx;
    KeepaliveAction::Wait(until_ping.min(until_give_up))
}

/// Subscriber-side failures. `Shutdown` ends the loop and is only ever
/// produced by test scripts — the live transport retries forever.
#[derive(Debug, thiserror::Error)]
pub enum EventsError {
    #[error("events connect failed: {0}")]
    Connect(String),
    #[error("events subscriber shutdown")]
    Shutdown,
}

/// The connect side of the subscriber, abstracted for sockets-free tests.
/// `async fn` in a trait is fine here for the same reason as
/// [`crate::cloud::device_flow::DeviceTransport`]: always used through a
/// generic bound, never `dyn`.
#[allow(async_fn_in_trait)]
pub trait EventsTransport {
    type Stream: EventsStream;
    /// Open one subscription. `Err(Connect)` is a failed attempt (retried);
    /// `Err(Shutdown)` ends the loop.
    async fn connect(&self) -> Result<Self::Stream, EventsError>;
}

/// One open subscription: a stream of text frames until the peer closes.
#[allow(async_fn_in_trait)]
pub trait EventsStream {
    /// The next text frame, or `None` once the connection is closed/broken.
    async fn next_text(&mut self) -> Option<String>;
}

/// The refresh side: re-pull one family's records into the local cache.
/// `"*"` refreshes every family that has pull machinery.
#[allow(async_fn_in_trait)]
pub trait FamilyRefresher {
    async fn refresh(&self, family: &str);

    /// Apply one pushed envelope without a list round-trip (ws-delta-push).
    /// Default falls back to a family refresh, so fakes and older refreshers
    /// keep their behavior.
    async fn apply_delta(&self, family: &str, _envelope: &serde_json::Value) {
        self.refresh(family).await;
    }
}

/// Runs the tracker sweep when a doorbell rings.
///
/// A separate trait from [`FamilyRefresher`] because it is a different kind of
/// thing: a family refresh pulls OUR records from OUR cloud, while this goes
/// and asks Linear or GitHub what changed. Keeping them apart means this file
/// stays ignorant of trackers — the implementation lives beside the CLI's
/// `tracker pull`, and calls the same sweep, so there is one classifier and one
/// ownership policy rather than two.
///
/// The default body does nothing, so a build with no tracker configured (and
/// every existing test fake) is unaffected.
/// Unlike the sibling traits here, the future is explicitly `Send`. It has to
/// be: this one is driven from a `tokio::spawn`ed task through a generic bound,
/// so the compiler only sees the trait's opaque future and cannot infer it.
/// The bound is also a useful constraint on implementors — it rules out holding
/// a `MutexGuard` across the sweep's network call, which is exactly the mistake
/// the shared-engine design would have invited.
pub trait TrackerSweeper {
    /// Re-check `provider` now. Errors are the implementation's to log: a
    /// doorbell is an optimization, and a failed one must degrade to the next
    /// `tracker pull` rather than propagate into the subscriber loop.
    fn sweep(&self, provider: &str) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let _ = provider;
        }
    }
}

/// How long the unattended sweep waits between runs.
///
/// Fifteen minutes, chosen against what a sweep actually costs rather than for
/// tidiness: one sweep is a single `fetch_since`, worth a handful of points
/// against GitHub's documented 900/minute REST budget, so four an hour is
/// nowhere near anything. It is deliberately far slower than the doorbell —
/// this is the FLOOR, the thing that holds when no webhook ever arrives, and a
/// floor does not need to be fast to be a floor.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(900);

/// Projects the roadmap OUT to the tracker on a cadence (tracker-auto-push).
///
/// The write-side twin of [`TrackerSweeper`], and a separate trait for the same
/// reason that one is separate from [`FamilyRefresher`]: these are different
/// KINDS of act. A sweep reads somebody else's system; this one writes to it.
/// Keeping them apart means the posture difference stays visible in the types —
/// a build may reasonably want the reading floor and not the writing one, which
/// is exactly the default (see `THINK_AND_SHIP_TRACKER_PUSH_SECS`).
///
/// Same `Send` bound as the sweeper, for the same reason: driven from a
/// `tokio::spawn`ed task through a generic bound, so the compiler only sees the
/// opaque future. The default body does nothing, so every existing test fake and
/// any build without a tracker is unaffected.
pub trait TrackerPusher {
    /// Project everything opted in to `provider` now. Errors are the
    /// implementation's to log: an unattended push is a convenience over
    /// `tracker push`, and a failed one must degrade to the next cycle rather
    /// than take down the task that carries it.
    fn push(&self, provider: &str) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let _ = provider;
        }
    }
}

/// Run the tracker push on a cadence, forever, so the plan reaches the tracker
/// without a human typing a command.
///
/// Deliberately the same shape as [`run_sweep_schedule`], including sleeping
/// BEFORE the first run — a server restarts once per client session, and
/// pushing on start would fire on every restart and have several clients
/// starting together all write to the tracker at once. `tracker push` remains
/// the answer to "I want it now".
///
/// There is NO default interval, unlike the sweep. Writing to somebody's real
/// tracker on a timer is a different posture from reading on one, so this only
/// ever runs when a human set the env var. See `parse_push_interval`.
///
/// `max_cycles` exists only so a test can watch the cadence without waiting —
/// production passes `None`, exactly as the sweep does.
pub async fn run_push_schedule<P: TrackerPusher, S: Sleeper>(
    provider: &str,
    interval: Duration,
    pusher: &P,
    sleeper: &S,
    max_cycles: Option<u32>,
) {
    let mut cycles: u32 = 0;
    loop {
        if max_cycles.is_some_and(|max| cycles >= max) {
            return;
        }
        sleeper.sleep(interval).await;
        pusher.push(provider).await;
        cycles += 1;
    }
}

/// Run the tracker sweep on a cadence, forever, so convergence does not depend
/// on a human remembering.
///
/// This closes a gap the doorbell left: `tracker pull` made the sweep runnable and
/// the doorbell made it SOONER, but with no webhook and nobody typing, nothing
/// converged at all. A doorbell cannot supply a floor — it only reacts.
///
/// It sleeps BEFORE the first sweep. An MCP server restarts once per client
/// session, so sweeping on start would fire on every restart and, worse, would
/// have several clients starting together all reach for the tracker at once.
/// The "I want it now" case already has two answers that are better than a
/// boot-time surprise: the doorbell, and `tracker pull`.
///
/// `max_cycles` exists only so a test can watch the cadence without waiting a
/// quarter of an hour — production passes `None`, the same role
/// [`EventsError::Shutdown`] plays for the subscriber loop.
pub async fn run_sweep_schedule<W: TrackerSweeper, S: Sleeper>(
    provider: &str,
    interval: Duration,
    sweeper: &W,
    sleeper: &S,
    max_cycles: Option<u32>,
) {
    let mut cycles: u32 = 0;
    loop {
        if max_cycles.is_some_and(|max| cycles >= max) {
            return;
        }
        sleeper.sleep(interval).await;
        sweeper.sweep(provider).await;
        cycles += 1;
    }
}

/// Drive the subscribe → notify → refresh loop forever (until a test script
/// signals shutdown).
///
/// Durability properties: every successful (re)connect starts with a full
/// `"*"` refresh, so notifications missed while disconnected are never lost;
/// after [`FALLBACK_AFTER_FAILURES`] consecutive connect failures, every retry
/// cycle polls (full refresh) so the cache converges even if the WS never
/// comes back. A closed stream counts as one failure, so reconnects after a
/// drop back off instead of spinning.
pub async fn run_events_loop<T: EventsTransport, R: FamilyRefresher, S: Sleeper>(
    transport: &T,
    refresher: &R,
    sleeper: &S,
) {
    run_events_loop_with(transport, refresher, &NoSweeper, sleeper).await;
}

/// A sweeper that does nothing — what [`run_events_loop`] uses, so every
/// existing caller and test keeps its exact behaviour.
pub struct NoSweeper;
impl TrackerSweeper for NoSweeper {}

/// [`run_events_loop`] plus a doorbell handler. See that function for the
/// durability properties, which are unchanged: a doorbell only ever makes a
/// sweep happen SOONER, so losing every frame costs latency and nothing else.
pub async fn run_events_loop_with<
    T: EventsTransport,
    R: FamilyRefresher,
    W: TrackerSweeper,
    S: Sleeper,
>(
    transport: &T,
    refresher: &R,
    sweeper: &W,
    sleeper: &S,
) {
    let mut failures: u32 = 0;
    loop {
        match transport.connect().await {
            Ok(mut stream) => {
                refresher.refresh("*").await;
                while let Some(text) = stream.next_text().await {
                    if let Some(note) = parse_notification(&text) {
                        match &note.envelope {
                            Some(env) => refresher.apply_delta(&note.family, env).await,
                            None => refresher.refresh(&note.family).await,
                        }
                    } else if let Some(news) = parse_tracker_news(&text) {
                        // `else if` deliberately: a frame is one or the other,
                        // and trying both would let a malformed record frame
                        // fall through into an outbound tracker call.
                        sweeper.sweep(&news.provider).await;
                    }
                }
                // A session both resets the failure streak and counts its own
                // drop as the first failure of the next one.
                failures = 1;
            }
            Err(EventsError::Shutdown) => return,
            Err(EventsError::Connect(e)) => {
                failures += 1;
                tracing::debug!(
                    target: "think_and_ship::cloud",
                    "events connect failed (attempt {failures}): {e}"
                );
            }
        }
        match after_failure(failures) {
            CycleAction::Sleep(d) => sleeper.sleep(d).await,
            CycleAction::PollAndSleep(d) => {
                refresher.refresh("*").await;
                sleeper.sleep(d).await;
            }
        }
    }
}

// ── live implementations ────────────────────────────────────────────────────

/// The live WS transport: `wss://…/v1/events` with `Authorization: Bearer`.
pub struct WsTransport {
    url: String,
    token: String,
}

impl WsTransport {
    /// Build from the cloud base URL (`https://…`) + Bearer token. http→ws /
    /// https→wss, mirroring the SPA's `toWsUrl`.
    #[must_use]
    pub fn new(base_url: &str, token: &str) -> Self {
        let ws_base = base_url.trim_end_matches('/').replacen("http", "ws", 1);
        Self {
            url: format!("{ws_base}/v1/events"),
            token: token.to_string(),
        }
    }
}

impl EventsTransport for WsTransport {
    type Stream = WsStream;

    async fn connect(&self) -> Result<Self::Stream, EventsError> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut request = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| EventsError::Connect(e.to_string()))?;
        let bearer = format!("Bearer {}", self.token)
            .parse()
            .map_err(|_| EventsError::Connect("token is not a valid header value".into()))?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            bearer,
        );
        let (stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| EventsError::Connect(e.to_string()))?;
        let now = std::time::Instant::now();
        Ok(WsStream {
            inner: stream,
            last_rx: now,
            last_ping: now,
        })
    }
}

/// One live subscription over tungstenite, with keepalive: quiet connections
/// are pinged every [`PING_INTERVAL`]; a connection silent (no frames, no
/// pongs) for [`IDLE_GIVE_UP`] is reported closed so the loop reconnects —
/// the zombie-socket fix. The policy itself is the pure
/// [`keepalive_action`]; this is only the IO glue.
pub struct WsStream {
    inner: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    last_rx: std::time::Instant,
    last_ping: std::time::Instant,
}

impl EventsStream for WsStream {
    async fn next_text(&mut self) -> Option<String> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        loop {
            let wait = match keepalive_action(self.last_rx.elapsed(), self.last_ping.elapsed()) {
                KeepaliveAction::GiveUp => return None, // silently dead — drop → reconnect
                KeepaliveAction::SendPing => {
                    self.last_ping = std::time::Instant::now();
                    if self
                        .inner
                        .send(Message::Ping(Vec::new().into()))
                        .await
                        .is_err()
                    {
                        return None; // the write surfaced the broken connection
                    }
                    continue;
                }
                KeepaliveAction::Wait(d) => d,
            };
            match tokio::time::timeout(wait, self.inner.next()).await {
                Err(_elapsed) => continue, // idle window over — re-consult the policy
                Ok(None) => return None,
                Ok(Some(Ok(Message::Text(text)))) => {
                    self.last_rx = std::time::Instant::now();
                    return Some(text.to_string());
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) => return None,
                Ok(Some(Ok(_))) => {
                    // Pong/ping/binary — not ours, but proof of life.
                    self.last_rx = std::time::Instant::now();
                    continue;
                }
            }
        }
    }
}

/// The live refresher: re-pulls a family through `cloud::pull` into the
/// shared engines. Fetches BEFORE locking and applies synchronously, so an
/// engine mutex is never held across an await (the guard wouldn't be `Send`,
/// and a stuck fetch must never block MCP tool calls).
pub struct EngineRefresher {
    client: CloudClient,
    think: Arc<Mutex<ReasoningServer>>,
    roadmap: Arc<Mutex<RoadmapEngine>>,
    signal: Arc<Mutex<SignalEngine>>,
    /// Per-family change cursors (cloud-read-amplification): after the first
    /// full pull, refreshes only fetch records with `updated >=` watermark, so
    /// steady-state rows-read is near-zero. Every FULL_REFRESH_EVERY-th
    /// refresh per family drops the cursor — the safety net for deletes and
    /// wipes, which a since-cursor can never observe.
    watermarks: Mutex<std::collections::HashMap<&'static str, String>>,
    refreshes: std::sync::atomic::AtomicU64,
}

/// Every Nth per-family refresh is a full pull (delete/wipe safety net).
const FULL_REFRESH_EVERY: u64 = 20;

impl EngineRefresher {
    #[must_use]
    pub fn new(
        client: CloudClient,
        think: Arc<Mutex<ReasoningServer>>,
        roadmap: Arc<Mutex<RoadmapEngine>>,
        signal: Arc<Mutex<SignalEngine>>,
    ) -> Self {
        Self {
            client,
            think,
            roadmap,
            signal,
            watermarks: Mutex::new(std::collections::HashMap::new()),
            refreshes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The cursor to use for this refresh: `None` (full pull) on the first
    /// fetch and every safety-net cycle, otherwise the family's watermark.
    fn cursor_for(&self, family: &'static str) -> Option<String> {
        let n = self
            .refreshes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n.is_multiple_of(FULL_REFRESH_EVERY) {
            return None;
        }
        self.watermarks
            .lock()
            .ok()
            .and_then(|w| w.get(family).cloned())
    }

    /// Advance the family's watermark to the newest stamp in the batch.
    fn advance(&self, family: &'static str, envelopes: &[serde_json::Value]) {
        if let (Some(max), Ok(mut w)) = (pull::max_watermark(envelopes), self.watermarks.lock()) {
            let entry = w.entry(family).or_default();
            if max > *entry {
                *entry = max;
            }
        }
    }

    /// Fetch one family honoring the cursor. Pure transport.
    async fn fetch(&self, family: &'static str) -> Result<Vec<serde_json::Value>, CloudError> {
        match self.cursor_for(family) {
            Some(since) => pull::fetch_family_since(&self.client, family, &since).await,
            None => pull::fetch_family(&self.client, family).await,
        }
    }

    async fn refresh_think(&self) {
        match self.fetch("think").await {
            Ok(envelopes) => {
                self.advance("think", &envelopes);
                if let Ok(mut engine) = self.think.lock() {
                    let adopted = pull::apply_think_records(&mut engine, &envelopes);
                    if adopted > 0 {
                        tracing::debug!(
                            target: "think_and_ship::cloud",
                            "realtime refresh adopted {adopted} think step(s)"
                        );
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "think_and_ship::cloud",
                "realtime think refresh failed: {e}"
            ),
        }
    }

    async fn refresh_roadmap(&self) {
        match self.fetch("roadmap").await {
            Ok(envelopes) => {
                self.advance("roadmap", &envelopes);
                if let Ok(mut engine) = self.roadmap.lock() {
                    let merged = pull::apply_roadmap_records(&mut engine, &envelopes);
                    if merged > 0 {
                        tracing::debug!(
                            target: "think_and_ship::cloud",
                            "realtime refresh merged {merged} roadmap record(s)"
                        );
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "think_and_ship::cloud",
                "realtime roadmap refresh failed: {e}"
            ),
        }
    }

    async fn refresh_signal(&self) {
        match self.fetch("signal").await {
            Ok(envelopes) => {
                self.advance("signal", &envelopes);
                if let Ok(mut engine) = self.signal.lock() {
                    let merged = pull::apply_signal_records(&mut engine, &envelopes);
                    if merged > 0 {
                        tracing::debug!(
                            target: "think_and_ship::cloud",
                            "realtime refresh merged {merged} signal record(s)"
                        );
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "think_and_ship::cloud",
                "realtime signal refresh failed: {e}"
            ),
        }
    }
}

impl FamilyRefresher for EngineRefresher {
    async fn refresh(&self, family: &str) {
        match family {
            "think" => self.refresh_think().await,
            "roadmap" => self.refresh_roadmap().await,
            "signal" => self.refresh_signal().await,
            "*" => {
                // Connectivity just proved itself (connect or poll cycle) —
                // drain any queued offline pushes BEFORE pulling, so our own
                // mutations land first and win recency (sync-offline-queue).
                self.client.flush_outbox().await;
                self.refresh_think().await;
                self.refresh_roadmap().await;
                self.refresh_signal().await;
            }
            // Ship notifications are deliberately ignored: the local ship
            // engine is a single-active-cycle tracker — hydrating foreign
            // cycles into it would fight the live objective. The cloud is the
            // cross-session read surface for ship history (sync-ship-full).
            other => tracing::debug!(
                target: "think_and_ship::cloud",
                "realtime notification for '{other}' ignored (no pull machinery yet)"
            ),
        }
    }

    /// ws-delta-push: merge the single pushed envelope through the same
    /// apply_* machinery the pull path uses — zero list round-trips. The
    /// watermark advances too, so the next safety-net pull stays incremental.
    async fn apply_delta(&self, family: &str, envelope: &serde_json::Value) {
        let one = std::slice::from_ref(envelope);
        // The watermark map keys on &'static str — resolve the static name.
        let fam: &'static str = match family {
            "think" => "think",
            "roadmap" => "roadmap",
            "signal" => "signal",
            other => {
                tracing::debug!(
                    target: "think_and_ship::cloud",
                    "delta for '{other}' ignored (no merge machinery)"
                );
                return;
            }
        };
        let merged = match fam {
            "think" => match self.think.lock() {
                Ok(mut engine) => pull::apply_think_records(&mut engine, one),
                Err(_) => return,
            },
            "roadmap" => match self.roadmap.lock() {
                Ok(mut engine) => pull::apply_roadmap_records(&mut engine, one),
                Err(_) => return,
            },
            "signal" => match self.signal.lock() {
                Ok(mut engine) => pull::apply_signal_records(&mut engine, one),
                Err(_) => return,
            },
            _ => unreachable!("fam is one of the three merge families"),
        };
        self.advance(fam, one);
        if merged > 0 {
            tracing::debug!(
                target: "think_and_ship::cloud",
                "applied pushed {family} delta without a list call"
            );
        }
    }
}

/// The live sleeper (the `cli::connect` twin, private there).
struct TokioSleeper;

impl Sleeper for TokioSleeper {
    async fn sleep(&self, dur: Duration) {
        tokio::time::sleep(dur).await;
    }
}

/// Spawn the unattended sweep onto the current tokio runtime. Returns `false`
/// (no-op) outside a runtime, matching [`spawn_realtime`].
///
/// Deliberately NOT wired into `spawn_realtime`, even though that would have
/// been one line: the subscriber only runs when CLOUD SYNC is configured, and
/// the user who most needs a floor is the one running a tracker with no cloud
/// at all — they get no doorbell either. Gating the floor on the tracker rather
/// than on the cloud is the whole point of it being a separate task.
pub fn spawn_sweep_schedule<W: TrackerSweeper + Send + Sync + 'static>(
    provider: String,
    interval: Duration,
    sweeper: W,
) -> bool {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return false;
    };
    handle.spawn(async move {
        run_sweep_schedule(&provider, interval, &sweeper, &TokioSleeper, None).await;
    });
    true
}

/// Spawn the unattended PUSH cadence onto the current tokio runtime. Returns
/// `false` (no-op) outside a runtime, the same tolerance as its sweep twin, so
/// sync unit tests never panic. Fire-and-forget for the life of the process.
pub fn spawn_push_schedule<P: TrackerPusher + Send + Sync + 'static>(
    provider: String,
    interval: Duration,
    pusher: P,
) -> bool {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return false;
    };
    handle.spawn(async move {
        run_push_schedule(&provider, interval, &pusher, &TokioSleeper, None).await;
    });
    true
}

/// Spawn the realtime subscriber onto the current tokio runtime. Returns
/// `false` (no-op) when called outside a runtime — the same tolerance as the
/// engines' fire-and-forget cloud pushes, so sync unit tests never panic.
/// The task is fire-and-forget for the life of the process.
pub fn spawn_realtime<W: TrackerSweeper + Send + Sync + 'static>(
    client: &CloudClient,
    think: Arc<Mutex<ReasoningServer>>,
    roadmap: Arc<Mutex<RoadmapEngine>>,
    signal: Arc<Mutex<SignalEngine>>,
    sweeper: W,
) -> bool {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return false;
    };
    let transport = WsTransport::new(client.base_url(), client.token());
    let refresher = EngineRefresher::new(client.clone(), think, roadmap, signal);
    handle.spawn(async move {
        run_events_loop_with(&transport, &refresher, &sweeper, &TokioSleeper).await;
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    // ── pure parts ──────────────────────────────────────────────────────────

    #[test]
    fn parse_accepts_record_created_and_rejects_everything_else() {
        let note = parse_notification(
            r#"{"type":"record.created","family":"signal","kind":"signal","id":"s1","created":"t"}"#,
        )
        .expect("valid notification");
        assert_eq!(note.family, "signal");

        assert!(parse_notification(r#"{"type":"something.else","family":"signal"}"#).is_none());
        assert!(parse_notification("not json").is_none());
        assert!(parse_notification(r#"{"family":"signal"}"#).is_none());
    }

    #[test]
    fn parse_carries_the_pushed_envelope_and_tolerates_its_absence() {
        // ws-delta-push: newer servers attach the changed envelope.
        let with = parse_notification(
            r#"{"type":"record.created","family":"roadmap","envelope":{"family":"roadmap","kind":"chunk","id":"c1"}}"#,
        )
        .expect("valid");
        let env = with.envelope.expect("envelope present");
        assert_eq!(env["id"], "c1");

        // Older servers (no envelope field) still parse — fallback refresh path.
        let without = parse_notification(
            r#"{"type":"record.created","family":"roadmap","kind":"chunk","id":"c1","created":"t"}"#,
        )
        .expect("valid");
        assert!(without.envelope.is_none());
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(5), Duration::from_secs(30));
        assert_eq!(backoff_delay(100), Duration::from_secs(30), "capped");
    }

    #[test]
    fn keepalive_pings_a_quiet_connection_and_gives_up_on_total_silence() {
        let s = Duration::from_secs;
        // Fresh connection: wait until the first ping is due.
        assert_eq!(
            keepalive_action(s(0), s(0)),
            KeepaliveAction::Wait(PING_INTERVAL)
        );
        // Ping due (quiet for PING_INTERVAL since the last one).
        assert_eq!(keepalive_action(s(30), s(30)), KeepaliveAction::SendPing);
        // Pinged but still silent: wait only as long as the give-up allows.
        assert_eq!(
            keepalive_action(s(80), s(20)),
            KeepaliveAction::Wait(s(10)),
            "the sooner deadline (give-up in 10s) bounds the wait"
        );
        // Total silence past the limit — declare the zombie dead.
        assert_eq!(keepalive_action(s(90), s(0)), KeepaliveAction::GiveUp);
        assert_eq!(keepalive_action(s(300), s(300)), KeepaliveAction::GiveUp);
    }

    #[test]
    fn keepalive_never_gives_up_while_pongs_flow() {
        let s = Duration::from_secs;
        // A pong resets since_last_rx; even with pings long overdue the policy
        // probes rather than drops — no reconnect churn on quiet tenants.
        assert_eq!(keepalive_action(s(5), s(600)), KeepaliveAction::SendPing);
        assert!(matches!(
            keepalive_action(s(5), s(5)),
            KeepaliveAction::Wait(_)
        ));
    }

    #[test]
    fn fallback_kicks_in_after_consecutive_failures() {
        assert_eq!(after_failure(1), CycleAction::Sleep(Duration::from_secs(2)));
        assert_eq!(after_failure(2), CycleAction::Sleep(Duration::from_secs(4)));
        assert_eq!(
            after_failure(FALLBACK_AFTER_FAILURES),
            CycleAction::PollAndSleep(POLL_INTERVAL)
        );
        assert_eq!(after_failure(10), CycleAction::PollAndSleep(POLL_INTERVAL));
    }

    // ── scripted mocks (the device_flow pattern) ───────────────────────────

    enum Script {
        Frames(Vec<&'static str>),
        Fail,
        Shutdown,
    }

    struct ScriptedTransport {
        scripts: RefCell<VecDeque<Script>>,
    }

    impl ScriptedTransport {
        fn new(scripts: Vec<Script>) -> Self {
            Self {
                scripts: RefCell::new(scripts.into()),
            }
        }
    }

    struct ScriptedStream {
        frames: VecDeque<&'static str>,
    }

    impl EventsStream for ScriptedStream {
        async fn next_text(&mut self) -> Option<String> {
            self.frames.pop_front().map(str::to_string)
        }
    }

    impl EventsTransport for ScriptedTransport {
        type Stream = ScriptedStream;

        async fn connect(&self) -> Result<ScriptedStream, EventsError> {
            match self.scripts.borrow_mut().pop_front() {
                Some(Script::Frames(frames)) => Ok(ScriptedStream {
                    frames: frames.into(),
                }),
                Some(Script::Fail) => Err(EventsError::Connect("scripted failure".into())),
                Some(Script::Shutdown) | None => Err(EventsError::Shutdown),
            }
        }
    }

    struct RecordingRefresher {
        refreshed: RefCell<Vec<String>>,
    }

    impl RecordingRefresher {
        fn new() -> Self {
            Self {
                refreshed: RefCell::new(Vec::new()),
            }
        }
    }

    impl FamilyRefresher for RecordingRefresher {
        async fn refresh(&self, family: &str) {
            self.refreshed.borrow_mut().push(family.to_string());
        }
    }

    /// Records which providers a doorbell asked us to sweep. `Mutex` rather
    /// than `RefCell` because [`TrackerSweeper`]'s future is `Send`.
    struct RecordingSweeper {
        swept: Mutex<Vec<String>>,
    }

    impl RecordingSweeper {
        fn new() -> Self {
            Self {
                swept: Mutex::new(Vec::new()),
            }
        }
        fn seen(&self) -> Vec<String> {
            self.swept.lock().unwrap().clone()
        }
    }

    impl TrackerSweeper for RecordingSweeper {
        async fn sweep(&self, provider: &str) {
            self.swept.lock().unwrap().push(provider.to_string());
        }
    }

    struct RecordingSleeper {
        sleeps: RefCell<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn new() -> Self {
            Self {
                sleeps: RefCell::new(Vec::new()),
            }
        }
    }

    impl Sleeper for RecordingSleeper {
        async fn sleep(&self, dur: Duration) {
            self.sleeps.borrow_mut().push(dur);
        }
    }

    const SIGNAL_FRAME: &str =
        r#"{"type":"record.created","family":"signal","kind":"signal","id":"s1","created":"t"}"#;
    const ROADMAP_FRAME: &str =
        r#"{"type":"record.created","family":"roadmap","kind":"chunk","id":"c1","created":"t"}"#;

    #[tokio::test]
    async fn notifications_refresh_their_family_after_a_full_initial_refresh() {
        let transport = ScriptedTransport::new(vec![
            Script::Frames(vec![SIGNAL_FRAME, "garbage", ROADMAP_FRAME]),
            Script::Shutdown,
        ]);
        let refresher = RecordingRefresher::new();
        let sleeper = RecordingSleeper::new();

        run_events_loop(&transport, &refresher, &sleeper).await;

        assert_eq!(
            *refresher.refreshed.borrow(),
            vec!["*", "signal", "roadmap"],
            "connect refreshes everything, then each notification its family; garbage ignored"
        );
        // The stream drop counts as one failure → one backoff sleep.
        assert_eq!(*sleeper.sleeps.borrow(), vec![backoff_delay(1)]);
    }

    #[tokio::test]
    async fn reconnect_backs_off_then_falls_back_to_polling() {
        let transport = ScriptedTransport::new(vec![
            Script::Fail,
            Script::Fail,
            Script::Fail,
            Script::Shutdown,
        ]);
        let refresher = RecordingRefresher::new();
        let sleeper = RecordingSleeper::new();

        run_events_loop(&transport, &refresher, &sleeper).await;

        assert_eq!(
            *sleeper.sleeps.borrow(),
            vec![backoff_delay(1), backoff_delay(2), POLL_INTERVAL],
            "two backoffs, then the poll-fallback interval"
        );
        assert_eq!(
            *refresher.refreshed.borrow(),
            vec!["*"],
            "the fallback cycle polls everything; pure-backoff cycles don't"
        );
    }

    #[tokio::test]
    async fn a_successful_connect_resets_the_failure_count() {
        let transport = ScriptedTransport::new(vec![
            Script::Fail,
            Script::Fail,
            Script::Frames(vec![]), // reconnects fine (empty session)
            Script::Fail,           // then fails again — backoff restarts at 1
            Script::Shutdown,
        ]);
        let refresher = RecordingRefresher::new();
        let sleeper = RecordingSleeper::new();

        run_events_loop(&transport, &refresher, &sleeper).await;

        assert_eq!(
            *sleeper.sleeps.borrow(),
            vec![
                backoff_delay(1), // fail #1
                backoff_delay(2), // fail #2
                backoff_delay(1), // clean session dropped
                backoff_delay(2), // fail again — counts from the drop, not from 3
            ]
        );
    }

    #[test]
    fn ws_url_derives_from_the_http_base() {
        assert_eq!(
            WsTransport::new("https://api.example.com/", "t").url,
            "wss://api.example.com/v1/events"
        );
        assert_eq!(
            WsTransport::new("http://localhost:8787", "t").url,
            "ws://localhost:8787/v1/events"
        );
    }

    fn quiet_think() -> Arc<Mutex<ReasoningServer>> {
        let mut c = crate::think::config::ThinkConfig::default();
        c.display.color_output = false;
        Arc::new(Mutex::new(ReasoningServer::new(c)))
    }

    // ── the doorbell ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_doorbell_frame_sweeps_the_named_provider() {
        let transport = ScriptedTransport::new(vec![
            Script::Frames(vec![r#"{"type":"tracker.news","provider":"linear"}"#]),
            Script::Shutdown,
        ]);
        let refresher = RecordingRefresher::new();
        let sweeper = RecordingSweeper::new();
        let sleeper = RecordingSleeper::new();

        run_events_loop_with(&transport, &refresher, &sweeper, &sleeper).await;

        assert_eq!(
            sweeper.seen(),
            vec!["linear"],
            "the doorbell must ring once"
        );
        // It is a DOORBELL over the tracker, not a record change: it must not
        // masquerade as one and trigger a family re-pull. The only "*" here is
        // the unconditional refresh every connect already does.
        assert_eq!(*refresher.refreshed.borrow(), vec!["*"]);
    }

    #[tokio::test]
    async fn a_record_frame_never_rings_the_doorbell() {
        let transport = ScriptedTransport::new(vec![
            Script::Frames(vec![r#"{"type":"record.created","family":"roadmap"}"#]),
            Script::Shutdown,
        ]);
        let refresher = RecordingRefresher::new();
        let sweeper = RecordingSweeper::new();
        let sleeper = RecordingSleeper::new();

        run_events_loop_with(&transport, &refresher, &sweeper, &sleeper).await;

        assert!(
            sweeper.seen().is_empty(),
            "a record notification must not cause an outbound tracker call"
        );
        assert_eq!(*refresher.refreshed.borrow(), vec!["*", "roadmap"]);
    }

    #[tokio::test]
    async fn an_unknown_frame_is_still_ignored_by_both() {
        // The additive property, from the other side: adding tracker.news must
        // not have turned "ignore what you don't recognise" into "guess".
        let transport = ScriptedTransport::new(vec![
            Script::Frames(vec![
                r#"{"type":"something.new","provider":"linear"}"#,
                "not json at all",
                r#"{"type":"tracker.news"}"#, // no provider — malformed
            ]),
            Script::Shutdown,
        ]);
        let refresher = RecordingRefresher::new();
        let sweeper = RecordingSweeper::new();
        let sleeper = RecordingSleeper::new();

        run_events_loop_with(&transport, &refresher, &sweeper, &sleeper).await;

        assert!(sweeper.seen().is_empty());
        assert_eq!(*refresher.refreshed.borrow(), vec!["*"]);
    }

    #[tokio::test]
    async fn losing_every_doorbell_still_refreshes_by_the_fallback() {
        // The essential property, and the easiest one to lose silently: a
        // doorbell is an OPTIMIZATION. Here the WS never connects, so no frame is ever
        // delivered and the sweeper is never called — and the loop must still
        // converge on its own via the poll fallback.
        let transport = ScriptedTransport::new(vec![
            Script::Fail,
            Script::Fail,
            Script::Fail,
            Script::Shutdown,
        ]);
        let refresher = RecordingRefresher::new();
        let sweeper = RecordingSweeper::new();
        let sleeper = RecordingSleeper::new();

        run_events_loop_with(&transport, &refresher, &sweeper, &sleeper).await;

        assert!(
            sweeper.seen().is_empty(),
            "no frame arrived, so no doorbell should have rung"
        );
        assert_eq!(
            *refresher.refreshed.borrow(),
            vec!["*"],
            "the poll fallback still refreshed, so state converges without any doorbell"
        );
        assert_eq!(
            *sleeper.sleeps.borrow(),
            vec![backoff_delay(1), backoff_delay(2), POLL_INTERVAL]
        );
    }

    #[test]
    fn the_doorbell_frame_carries_no_issue_identity() {
        // Structural: TrackerNews has exactly the two fields. If somebody adds
        // an issue id here, the edge has begun holding link state it cannot
        // reason about, which is exactly the line this design draws.
        let news = parse_tracker_news(
            r#"{"type":"tracker.news","provider":"linear","identifier":"ENG-1","data":{"id":"x"}}"#,
        )
        .expect("parses");
        assert_eq!(news.provider, "linear");
        let src = include_str!("events.rs");
        let decl = src
            .split_once("pub struct TrackerNews {")
            .expect("TrackerNews must exist")
            .1
            .split_once('}')
            .expect("struct must close")
            .0;
        for forbidden in ["identifier", "external_id", "issue", "data"] {
            assert!(
                !decl.contains(forbidden),
                "TrackerNews gained a `{forbidden}` field — the doorbell must say \
                 only WHICH PROVIDER to re-check, never which item, or the edge \
                 starts holding link state the sweep is supposed to own"
            );
        }
    }

    // ── tracker-sweep-schedule: the convergence floor ───────────────────────

    #[tokio::test]
    async fn the_sweep_fires_unattended_on_the_cadence() {
        // CRITERION 3, demonstrated rather than asserted: nothing here delivers
        // a frame, nothing calls a command, and the sweep still runs — three
        // times, at the interval it was given.
        let sweeper = RecordingSweeper::new();
        let sleeper = RecordingSleeper::new();
        let every = Duration::from_secs(900);

        run_sweep_schedule("linear", every, &sweeper, &sleeper, Some(3)).await;

        assert_eq!(sweeper.seen(), vec!["linear", "linear", "linear"]);
        assert_eq!(*sleeper.sleeps.borrow(), vec![every, every, every]);
    }

    /// Records sleeps and sweeps into ONE ordered log, because the property
    /// under test is the ORDER. Two separate recorders can only prove counts,
    /// and a deliberate-breakage run showed counts alone let the sleep and the sweep swap
    /// places without any test noticing.
    struct Timeline {
        events: Mutex<Vec<String>>,
    }

    impl Timeline {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }
        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl Sleeper for Timeline {
        async fn sleep(&self, dur: Duration) {
            self.events
                .lock()
                .unwrap()
                .push(format!("sleep:{}", dur.as_secs()));
        }
    }

    impl TrackerSweeper for Timeline {
        async fn sweep(&self, provider: &str) {
            self.events
                .lock()
                .unwrap()
                .push(format!("sweep:{provider}"));
        }
    }

    impl TrackerPusher for Timeline {
        async fn push(&self, provider: &str) {
            self.events.lock().unwrap().push(format!("push:{provider}"));
        }
    }

    #[tokio::test]
    async fn it_waits_before_the_first_sweep_rather_than_firing_on_start() {
        // An MCP server restarts once per client session. Sweeping on start
        // would reach for the tracker on every restart, and would have several
        // clients starting together do it at once.
        let t = Timeline::new();

        run_sweep_schedule("linear", Duration::from_secs(900), &t, &t, Some(2)).await;

        assert_eq!(
            t.events(),
            vec!["sleep:900", "sweep:linear", "sleep:900", "sweep:linear"],
            "every sweep must be PRECEDED by its wait, not followed by it"
        );
    }

    #[tokio::test]
    async fn zero_cycles_sweeps_nothing_at_all() {
        // The shape the env off-switch relies on: no cycles means no outbound
        // call, not one call and then a stop.
        let sweeper = RecordingSweeper::new();
        let sleeper = RecordingSleeper::new();

        run_sweep_schedule(
            "linear",
            Duration::from_secs(1),
            &sweeper,
            &sleeper,
            Some(0),
        )
        .await;

        assert!(sweeper.seen().is_empty());
        assert!(sleeper.sleeps.borrow().is_empty());
    }

    // ── tracker-auto-push: the outbound cadence ─────────────────────────────

    #[tokio::test]
    async fn the_push_fires_unattended_on_the_cadence() {
        // The core requirement here, demonstrated by EXECUTION rather
        // than asserted from source: nobody types a command, nothing delivers a
        // frame, and the roadmap is projected three times anyway.
        let t = Timeline::new();
        let every = Duration::from_secs(300);

        run_push_schedule("linear", every, &t, &t, Some(3)).await;

        assert_eq!(
            t.events(),
            vec![
                "sleep:300",
                "push:linear",
                "sleep:300",
                "push:linear",
                "sleep:300",
                "push:linear"
            ],
            "the unattended push must run on its cadence, each run PRECEDED by \
             its wait — a push that fires on start would write to the tracker on \
             every server restart"
        );
    }

    #[tokio::test]
    async fn zero_cycles_pushes_nothing_at_all() {
        // What the default-off switch relies on: no cycles means no outbound
        // WRITE, not one write and then a stop. Stronger than the sweep's
        // equivalent because the failure here is a network write nobody asked
        // for, not a read.
        let t = Timeline::new();

        run_push_schedule("linear", Duration::from_secs(1), &t, &t, Some(0)).await;

        assert!(
            t.events().is_empty(),
            "a disabled push must not touch the tracker even once"
        );
    }

    #[tokio::test]
    async fn the_push_cadence_carries_the_provider_it_was_given() {
        // Pushing to the wrong tracker is not silent the way sweeping the wrong
        // one is — it CREATES ISSUES somewhere they do not belong.
        let t = Timeline::new();

        run_push_schedule("github", Duration::from_secs(60), &t, &t, Some(2)).await;

        assert_eq!(
            t.events(),
            vec!["sleep:60", "push:github", "sleep:60", "push:github"]
        );
    }

    #[tokio::test]
    async fn the_cadence_carries_the_provider_it_was_given() {
        // Guards the one thing a copy-paste would break: sweeping the wrong
        // tracker is silent — it just never finds anything.
        let sweeper = RecordingSweeper::new();
        let sleeper = RecordingSleeper::new();

        run_sweep_schedule(
            "github",
            Duration::from_secs(60),
            &sweeper,
            &sleeper,
            Some(2),
        )
        .await;

        assert_eq!(sweeper.seen(), vec!["github", "github"]);
    }

    #[test]
    fn the_default_cadence_is_slow_enough_to_be_a_floor() {
        // Not a magic-number test: a sweep is one fetch_since against a budget
        // of 900 points/minute, so the failure mode worth guarding is somebody
        // "improving" this to seconds and turning a backstop into a hammer.
        assert!(
            SWEEP_INTERVAL >= Duration::from_secs(300),
            "the floor must stay far slower than the doorbell; \
             a fast default would burn the tracker's rate budget for no latency \
             win the doorbell does not already provide"
        );
    }

    #[test]
    fn spawn_outside_a_runtime_is_a_noop() {
        let client = CloudClient::new("https://example.test", "tok");
        let roadmap = Arc::new(Mutex::new(RoadmapEngine::new("p".into())));
        let signal = Arc::new(Mutex::new(SignalEngine::new("p".into())));
        assert!(!spawn_realtime(
            &client,
            quiet_think(),
            roadmap,
            signal,
            NoSweeper
        ));
    }

    #[tokio::test]
    async fn spawn_inside_a_runtime_starts_the_task() {
        let client = CloudClient::new("https://localhost:1", "tok"); // unreachable — task just retries
        let roadmap = Arc::new(Mutex::new(RoadmapEngine::new("p".into())));
        let signal = Arc::new(Mutex::new(SignalEngine::new("p".into())));
        assert!(spawn_realtime(
            &client,
            quiet_think(),
            roadmap,
            signal,
            NoSweeper
        ));
    }
}
