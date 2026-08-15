//! LIVE LOG emission — the signal that makes a logs platform worth running.
//!
//! # Why this exists when the span lane already works
//!
//! Measured on the live HyperDX before this module existed: `SELECT count()
//! FROM default.otel_logs` returned 0 while `otel_traces` held 227. Trace-to-log
//! correlation — click a span, read the log lines emitted INSIDE it — is the one
//! capability HyperDX/ClickStack, SigNoz and Grafana's LGTM stack have that
//! Jaeger does not. Sending only spans buys a strictly worse Jaeger on a heavier
//! stack.
//!
//! # The four decisions, made in writing
//!
//! **Correlation is a scope guard, not a restructure.** A real restructure
//! looked necessary at first: correlation needs the current span's ids at
//! log time, and [`crate::otel_live::LiveEmitter::tool_call`] is called AFTER
//! the call returns, so there is no ambient current span. That is true of the
//! span OBJECT and false of the span ID — the id is
//! `span_id("mcp.call", "{project}:{tool}:{start_nanos}")`, and every input is
//! known BEFORE the await. Only the end timestamp and the error code are
//! post-await facts, and neither participates in the id. So the correlating pair
//! is published at call start by [`in_call`] while the span goes on being built
//! at call end, unchanged.
//!
//! It is a `tokio::task_local!` and NOT a `thread_local!`, which is load-
//! bearing: `route` awaits its dispatch, and on a multi-threaded runtime a task
//! can resume on a different worker thread than it suspended on. A thread-local
//! would attach logs to the WRONG call's span under concurrency — worse than no
//! correlation, because it looks like it works.
//!
//! **Stderr is DUPLICATED, never replaced.** Stderr is the only channel when no
//! endpoint is configured. Replacing it would mean configuring telemetry
//! silently DELETES the local diagnostic — the operator gains a remote view,
//! loses the one they had, and finds out during the incident. This layer is
//! added beside the existing `fmt` layer, which is untouched.
//!
//! **A `tracing` Layer, fenced on two axes.** The filing chunk framed a Layer as
//! "every existing log for free, which is both the cheap answer and the risky
//! one". Counting the tree falsified the premise: 39 `tracing::warn!`, 9
//! `info!`, 15 `debug!` and zero `error!` — against 91 `eprintln!`, which a
//! Layer cannot see at all and which are mostly CLI output for a human at a
//! terminal. So the Layer is not "everything", it is exactly the operational
//! half; and the risk is bounded by requiring `TARGET_FENCE` and WARN+. The
//! `otel-eprintln-to-tracing` chunk has since moved 34 of those `eprintln!`
//! sites — every one that reports a failure of our own side effects — onto
//! `tracing::warn!`, so the Layer now sees them. The rest stayed deliberately:
//! CLI output, and a third population of SUCCESS NARRATION that `info!` would
//! delete twice over (below the default `EnvFilter("warn")` on stderr, below
//! this fence for OTLP). `tests/log_lane_boundary.rs` states that rule and
//! holds the boundary in both directions.
//!
//! target fence also structurally excludes reqwest/hyper/rustls, whose debug
//! logs carry URLs with credentials in them — and stops this module's own HTTP
//! client from recursing into the layer that feeds it.
//!
//! **Redaction is ASYMMETRIC, and deliberately not inherited.** The
//! `saas-anon-telemetry` lane redacts at source because its destination is OUR
//! backend receiving OTHER people's data: the operator is the subject, not the
//! audience. Here the relationship is inverted — the destination is the endpoint
//! the OPERATOR configured, about the operator's own workload, and they
//! configured it precisely to see which project's sweep failed. Redacting the
//! project name or the chunk title would destroy the entire payload in order to
//! protect someone from their own data. So domain content is emitted VERBATIM.
//!
//! Credentials get the opposite answer, because a token is never the payload:
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is as often Honeycomb or Grafana Cloud as a
//! local container, and log lines get screenshotted and forwarded. [`redact`]
//! scrubs credential-SHAPED substrings unconditionally, on the way out.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use serde_json::{Value, json};
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::otel_live::now_nanos;

/// Only events whose target starts with this reach the wire.
///
/// This is the whole answer to "a Layer would ship whatever anyone ever logged":
/// "anyone" is fenced to us. Rust's module paths make our crate's default target
/// `think_and_ship::…`, and the handful of sites that set an explicit target
/// (`target: "think_and_ship::cloud"`) already spell it the same way.
const TARGET_FENCE: &str = "think_and_ship";

/// How many records may sit unsent before new ones are dropped. Same bound and
/// same reason as the span lane: a dead collector must cost bounded memory.
const QUEUE_CAPACITY: usize = 2048;
/// How long the worker waits for more records before POSTing what it has.
const BATCH_INTERVAL: Duration = Duration::from_millis(500);
/// How long a flush waits for the worker to drain. A shutdown that hangs on
/// telemetry is a worse bug than a lost log line.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// `SEVERITY_NUMBER_WARN` and `SEVERITY_NUMBER_ERROR` from the OTLP log data
/// model, read out of `opentelemetry-proto/…/logs/v1/logs.proto` rather than
/// remembered. A backend's "errors only" filter keys off this number, not off
/// the text beside it.
const SEVERITY_WARN: i64 = 13;
const SEVERITY_ERROR: i64 = 17;

/// The pair that ties a log record to the span it happened inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correlation {
    pub trace_id: String,
    pub span_id: String,
}

tokio::task_local! {
    /// The tool call the current task is executing, if any.
    static CURRENT_CALL: Correlation;
}

/// Run `fut` with `correlation` published to every log emitted inside it.
///
/// `None` runs the future unchanged — a call with live emission switched off is
/// not a call whose logs should be dropped, merely one whose logs have no span
/// to point at.
pub async fn in_call<F: Future>(correlation: Option<Correlation>, fut: F) -> F::Output {
    match correlation {
        Some(c) => CURRENT_CALL.scope(c, fut).await,
        None => fut.await,
    }
}

/// The tool call the current task is executing, if any. `None` off a tool call
/// — a background sweep's warning is still worth sending, just uncorrelated.
#[must_use]
pub fn current() -> Option<Correlation> {
    CURRENT_CALL.try_with(Clone::clone).ok()
}

/// Scrub credential-shaped substrings from a log body.
///
/// Deliberately NOT a general redactor: domain content (project names, chunk
/// titles, tracker URLs) passes through untouched, because that is the payload
/// the operator configured an endpoint to receive. What is removed is only what
/// can never be payload — a bearer token, a secret-shaped query parameter, URL
/// userinfo, and the vendor token prefixes this project actually handles.
///
/// Errs toward over-scrubbing within those shapes: a redacted string that a
/// human could have read is an inconvenience, a leaked Linear API key in a
/// screenshot is not.
#[must_use]
pub fn redact(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    // Advance by CHARACTERS, never by bytes: every `i` is a char boundary by
    // construction, so a non-ASCII log line can neither panic the redactor nor
    // be cut in half by it.
    while let Some(ch) = body[i..].chars().next() {
        if let Some(consumed) = scrub_at(&body[i..], &mut out) {
            i += consumed;
        } else {
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// The token-bearing shapes, tried at one position. Returns how many bytes were
/// consumed when one matched.
fn scrub_at(rest: &str, out: &mut String) -> Option<usize> {
    // `Authorization: Bearer <token>` and bare `Bearer <token>`.
    for lead in ["Bearer ", "bearer "] {
        if let Some(tail) = rest.strip_prefix(lead) {
            let len = value_len(tail);
            if len > 0 {
                out.push_str(lead);
                out.push_str(REDACTED);
                return Some(lead.len() + len);
            }
        }
    }
    // Sensitive key/value pairs, in a query string or in prose. `get` rather
    // than indexing: a key length can land inside a multi-byte character, and
    // `&rest[..n]` would panic on the log line rather than redact it.
    for key in SENSITIVE_KEYS {
        if let Some(head) = rest.get(..key.len())
            && head.eq_ignore_ascii_case(key)
            && let Some(tail) = rest.get(key.len()..)
            && let Some(sep) = tail.chars().next()
            && (sep == '=' || sep == ':')
        {
            let value = &tail[sep.len_utf8()..];
            // A `:` with a space after it is prose ("token: missing"), not an
            // assignment — scrubbing there would eat readable diagnostics.
            let assignment = sep == '=' || !value.starts_with(' ');
            let len = value_len(value);
            if assignment && len > 0 {
                out.push_str(head);
                out.push(sep);
                out.push_str(REDACTED);
                return Some(key.len() + sep.len_utf8() + len);
            }
        }
    }
    // URL userinfo: `scheme://user:pass@host` — the password is in the URL, so
    // the whole userinfo goes.
    if rest.starts_with("://")
        && let Some(tail) = rest.get(3..)
    {
        let stop = tail
            .find(|c: char| c.is_whitespace() || c == '/' || c == '"')
            .unwrap_or(tail.len());
        if let Some(at) = tail[..stop].find('@')
            && tail[..at].contains(':')
        {
            out.push_str("://");
            out.push_str(REDACTED);
            out.push('@');
            return Some(3 + at + 1);
        }
    }
    // Vendor token prefixes this project genuinely handles. A token is
    // recognisable on its own, with no key beside it, once it is pasted into a
    // message like "GET failed for lin_api_…".
    for prefix in TOKEN_PREFIXES {
        if rest.starts_with(prefix) {
            let len = value_len(rest);
            // Only if something actually follows the prefix.
            if len > prefix.len() {
                out.push_str(prefix);
                out.push_str(REDACTED);
                return Some(len);
            }
        }
    }
    None
}

const REDACTED: &str = "[REDACTED]";

/// Keys whose value is a credential by definition. Matched case-insensitively.
const SENSITIVE_KEYS: &[&str] = &[
    "authorization",
    "access_token",
    "refresh_token",
    "client_secret",
    "api_key",
    "apikey",
    "password",
    "secret",
    "token",
    "key",
];

/// Prefixes that identify a credential with no key beside it.
const TOKEN_PREFIXES: &[&str] = &[
    "lin_api_",
    "lin_oauth_",
    "github_pat_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "xoxb-",
    "xoxp-",
    "sk-",
];

/// How far a credential value runs: to the first delimiter that cannot be part
/// of one.
///
/// An HTTP auth scheme is an exception, and a leak until it is handled:
/// `Authorization=Basic aGk6dGhlcmU=` has a SPACE inside the credential, so
/// stopping at the first delimiter scrubs the word "Basic" and ships the
/// base64'd `user:password` beside it. The scheme is never the secret, so it
/// does not terminate the value.
fn value_len(s: &str) -> usize {
    for scheme in AUTH_SCHEMES {
        if let Some(head) = s.get(..scheme.len())
            && head.eq_ignore_ascii_case(scheme)
        {
            return scheme.len() + plain_value_len(&s[scheme.len()..]);
        }
    }
    plain_value_len(s)
}

/// Auth schemes that precede the credential rather than being it. The trailing
/// space is part of the match: `Bearerish` is not a scheme.
const AUTH_SCHEMES: &[&str] = &["basic ", "bearer ", "token ", "digest "];

fn plain_value_len(s: &str) -> usize {
    s.find(|c: char| {
        c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ',' | ';' | ')' | '}' | '>')
    })
    .unwrap_or(s.len())
}

/// What the log lane needs to reach a backend. `None` anywhere means "not
/// configured", which is the enable gate — no endpoint, no thread, no network.
#[derive(Debug, Clone)]
pub struct LogConfig {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub project: String,
}

impl LogConfig {
    /// Read the environment. `None` — meaning no log emission at all — unless a
    /// logs endpoint resolves.
    #[must_use]
    pub fn from_env(project: &str) -> Option<Self> {
        Some(Self {
            endpoint: crate::otlp_config::configured_logs_endpoint()?,
            headers: crate::otlp_config::configured_logs_headers().unwrap_or_default(),
            project: project.to_string(),
        })
    }
}

enum Msg {
    Record(Value),
    Flush(SyncSender<()>),
}

/// The live log lane. Cheap to clone; every clone feeds one worker.
#[derive(Clone)]
pub struct LogEmitter {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    tx: SyncSender<Msg>,
    project: String,
    dropped: Arc<AtomicU64>,
}

impl LogEmitter {
    /// A disabled emitter. Every method is a no-op; no thread is spawned. This
    /// is what runs on every machine that never configured OTLP.
    #[must_use]
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// Build from the environment, spawning the worker only if configured.
    #[must_use]
    pub fn from_env(project: &str) -> Self {
        LogConfig::from_env(project).map_or_else(Self::disabled, Self::new)
    }

    /// Build with an explicit config, spawning the worker.
    #[must_use]
    pub fn new(config: LogConfig) -> Self {
        let (tx, rx) = sync_channel(QUEUE_CAPACITY);
        let cfg = config.clone();
        // A plain thread owning its own runtime — the same pattern the span lane
        // uses, and for the same reason: `init_tracing` runs before any ambient
        // tokio runtime is guaranteed to exist.
        std::thread::Builder::new()
            .name("otel-logs".into())
            .spawn(move || worker(&rx, &cfg))
            .ok();
        Self {
            inner: Some(Arc::new(Inner {
                tx,
                project: config.project,
                dropped: Arc::new(AtomicU64::new(0)),
            })),
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// How many records were dropped for a full queue.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.inner
            .as_ref()
            .map_or(0, |i| i.dropped.load(Ordering::Relaxed))
    }

    /// Enqueue one log record, correlated to the current tool call if there is
    /// one. Never blocks: a full queue drops and counts.
    pub fn record(&self, level: &tracing::Level, target: &str, body: &str) {
        let Some(inner) = &self.inner else { return };
        inner.enqueue(Msg::Record(log_record(
            level,
            target,
            body,
            &inner.project,
            current().as_ref(),
        )));
    }

    /// Drain the queue and POST what is in it.
    pub fn flush(&self) {
        let Some(inner) = &self.inner else { return };
        let (ack_tx, ack_rx) = sync_channel(1);
        if inner.tx.try_send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(FLUSH_TIMEOUT);
        }
    }
}

impl Inner {
    fn enqueue(&self, msg: Msg) {
        // Full or disconnected: DROP. Blocking here would block the code that
        // logged, which is the rule the span lane already follows — nothing
        // about observability is worth stalling the work it describes.
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = self.tx.try_send(msg) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Build one OTLP log record. Pure, so every clause can be asserted on the JSON
/// without a worker or a socket.
fn log_record(
    level: &tracing::Level,
    target: &str,
    body: &str,
    project: &str,
    correlation: Option<&Correlation>,
) -> Value {
    let now = now_nanos().to_string();
    let mut record = json!({
        "timeUnixNano": now,
        "observedTimeUnixNano": now,
        "severityNumber": severity_number(level),
        "severityText": level.as_str(),
        // REDACTED HERE, on the way out — the single seam every body crosses,
        // so a future call site cannot forget to ask.
        "body": { "stringValue": redact(body) },
        "attributes": [
            crate::otel::attr("think_and_ship.project", project),
            crate::otel::attr("code.namespace", target),
        ],
    });
    if let Some(c) = correlation {
        record["traceId"] = json!(c.trace_id);
        record["spanId"] = json!(c.span_id);
        // The W3C sampled bit, which is what a backend reads to decide whether
        // the trace this record names is one it kept.
        record["flags"] = json!(1);
    }
    record
}

fn severity_number(level: &tracing::Level) -> i64 {
    if *level == tracing::Level::ERROR {
        SEVERITY_ERROR
    } else {
        SEVERITY_WARN
    }
}

/// Wrap records in the OTLP/HTTP envelope. The SAME resource attributes the span
/// lanes emit, which is what makes a backend file both signals under one service
/// and offer the click-through at all.
fn envelope(project: &str, records: Vec<Value>) -> Value {
    json!({
        "resourceLogs": [{
            "resource": {
                "attributes": [
                    crate::otel::attr("service.name", "think-and-ship"),
                    crate::otel::attr("gen_ai.system", "think-and-ship"),
                    crate::otel::attr("think_and_ship.project", project),
                ]
            },
            "scopeLogs": [{
                "scope": { "name": "think-and-ship", "version": env!("CARGO_PKG_VERSION") },
                "logRecords": records,
            }]
        }]
    })
}

/// The background worker: batch, POST, repeat. Every error is swallowed — this
/// process may be serving MCP on stdio, and a collector that is down must not
/// write to the transport the client is parsing.
fn worker(rx: &Receiver<Msg>, cfg: &LogConfig) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let Some(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()
    else {
        return;
    };

    let mut batch: Vec<Value> = Vec::new();
    loop {
        match rx.recv_timeout(BATCH_INTERVAL) {
            Ok(Msg::Record(r)) => {
                batch.push(r);
                while let Ok(Msg::Record(more)) = rx.try_recv() {
                    batch.push(more);
                }
            }
            Ok(Msg::Flush(ack)) => {
                post(&runtime, &client, cfg, std::mem::take(&mut batch));
                let _ = ack.send(());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                post(&runtime, &client, cfg, std::mem::take(&mut batch));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                post(&runtime, &client, cfg, std::mem::take(&mut batch));
                return;
            }
        }
    }
}

fn post(
    runtime: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    cfg: &LogConfig,
    records: Vec<Value>,
) {
    if records.is_empty() {
        return;
    }
    let body = envelope(&cfg.project, records);
    runtime.block_on(async {
        let mut request = client.post(&cfg.endpoint).json(&body);
        for (key, value) in &cfg.headers {
            request = request.header(key, value);
        }
        // Deliberately unexamined, and NOT logged: reporting a failed log POST
        // through `tracing` would feed this very layer, and a collector that is
        // down would produce an unbounded self-amplifying loop.
        let _ = request.send().await;
    });
}

/// The process-wide lane the `tracing` layer feeds.
///
/// A global because `init_tracing` runs long before `build_unified` constructs
/// the MCP service, so the layer cannot be handed an emitter it does not yet
/// have. Written once at startup.
static EMITTER: std::sync::OnceLock<LogEmitter> = std::sync::OnceLock::new();

/// Install the process-wide log lane. Returns the layer to add beside the
/// existing `fmt` layer — beside, never instead of (see the module docs).
#[must_use]
pub fn install(project: &str) -> OtelLogLayer {
    let _ = EMITTER.set(LogEmitter::from_env(project));
    OtelLogLayer
}

/// Drain whatever the lane is holding. Called on shutdown for the same reason
/// the span lane flushes: the last thing that happened is usually the
/// interesting one.
pub fn flush() {
    if let Some(e) = EMITTER.get() {
        e.flush();
    }
}

/// Is the log lane live? Used by reporting surfaces, not by the hot path.
#[must_use]
pub fn is_enabled() -> bool {
    EMITTER.get().is_some_and(LogEmitter::is_enabled)
}

/// The `tracing` layer that forwards our own warnings to OTLP.
///
/// Fenced on target and level — see `TARGET_FENCE` and the module docs for why
/// an unfenced layer was rejected even though it is the cheaper code.
#[derive(Debug, Clone, Copy)]
pub struct OtelLogLayer;

/// Pull the `message` field (what `tracing::warn!("…")` puts the text in) plus
/// any structured fields beside it, into one human-readable body.
#[derive(Default)]
struct BodyVisitor {
    message: String,
    fields: Vec<String>,
}

impl Visit for BodyVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

impl BodyVisitor {
    fn into_body(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields.join(" ")
        } else {
            format!("{} {}", self.message, self.fields.join(" "))
        }
    }
}

/// True when an event is one we send: our own target, at WARN or above.
#[must_use]
pub fn passes_fence(target: &str, level: &tracing::Level) -> bool {
    target.starts_with(TARGET_FENCE) && *level <= tracing::Level::WARN
}

impl<S: Subscriber> Layer<S> for OtelLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !passes_fence(meta.target(), meta.level()) {
            return;
        }
        let Some(emitter) = EMITTER.get() else { return };
        if !emitter.is_enabled() {
            return;
        }
        let mut visitor = BodyVisitor::default();
        event.record(&mut visitor);
        emitter.record(meta.level(), meta.target(), &visitor.into_body());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate. A disabled lane must spawn nothing and accept everything,
    /// because it is what runs on every machine that never configured OTLP.
    #[test]
    fn a_disabled_lane_is_inert() {
        let e = LogEmitter::disabled();
        assert!(!e.is_enabled());
        e.record(&tracing::Level::WARN, "think_and_ship::x", "anything");
        e.flush();
        assert_eq!(e.dropped(), 0);
    }

    /// THE FENCE, both halves. An unfenced layer was the cheap answer and this
    /// is what buying the expensive one bought: a dependency's debug log — the
    /// class most likely to contain a URL with a token in it — cannot reach the
    /// wire, and neither can our own chatter below WARN.
    #[test]
    fn the_fence_excludes_dependencies_and_everything_below_warn() {
        assert!(passes_fence(
            "think_and_ship::tracker",
            &tracing::Level::WARN
        ));
        assert!(passes_fence(
            "think_and_ship::cloud",
            &tracing::Level::ERROR
        ));
        // Dependencies, by target.
        assert!(!passes_fence("reqwest::connect", &tracing::Level::WARN));
        assert!(!passes_fence("hyper::client", &tracing::Level::ERROR));
        assert!(!passes_fence("rustls::conn", &tracing::Level::WARN));
        // Our own, by level.
        assert!(!passes_fence(
            "think_and_ship::cloud",
            &tracing::Level::INFO
        ));
        assert!(!passes_fence(
            "think_and_ship::cloud",
            &tracing::Level::DEBUG
        ));
        assert!(!passes_fence(
            "think_and_ship::cloud",
            &tracing::Level::TRACE
        ));
    }

    /// REDACTION, the "keep" half — and this is the decision, not an oversight.
    /// The operator configured an endpoint precisely to learn WHICH project's
    /// sweep failed and WHICH chunk it was about. A general redactor would
    /// deliver an empty diagnostic and call it privacy.
    #[test]
    fn domain_content_survives_redaction_intact() {
        let body = "tracker push failed for chunk otel-live-logs-signal in project \
                    think-and-ship-676f38: https://tracker.example.com/team/TAS/issue/TAS-1";
        assert_eq!(redact(body), body, "domain content is the payload");
    }

    /// REDACTION, the "scrub" half. A token is never the payload.
    #[test]
    fn credential_shapes_are_scrubbed() {
        for (input, must_not_contain) in [
            ("auth failed: Bearer eyJhbGciOi.secret.part", "eyJhbGciOi"),
            ("GET /x?api_key=abc123def&page=2", "abc123def"),
            ("header Authorization=Basic aGk6dGhlcmU=", "aGk6dGhlcmU"),
            ("linear rejected lin_api_9f8e7d6c5b4a", "9f8e7d6c5b4a"),
            ("push to https://user:hunter2@git.example/x", "hunter2"),
            ("client_secret=s3cr3t-value", "s3cr3t-value"),
        ] {
            let got = redact(input);
            assert!(
                !got.contains(must_not_contain),
                "{input:?} still leaks {must_not_contain:?}: {got:?}"
            );
            assert!(got.contains(REDACTED), "{input:?} -> {got:?}");
        }
    }

    /// REGRESSION, and a real leak the first version shipped: an HTTP auth
    /// scheme puts a SPACE inside the credential, so terminating the value at
    /// the first delimiter scrubbed the word "Basic" and sent the base64'd
    /// `user:password` immediately after it. The scheme is never the secret.
    #[test]
    fn an_auth_scheme_does_not_terminate_the_credential_after_it() {
        for input in [
            "header Authorization=Basic aGk6dGhlcmU=",
            "header authorization=bearer eyJhbGciOi",
            "x-api Token=Token abc123secret",
        ] {
            let got = redact(input);
            for leaked in ["aGk6dGhlcmU", "eyJhbGciOi", "abc123secret"] {
                assert!(!got.contains(leaked), "{input:?} leaks {leaked:?}: {got:?}");
            }
        }
    }

    /// A scrub that eats the surrounding sentence is a scrub nobody can act on.
    /// The non-secret half of the line must survive, or the operator gets a
    /// redaction marker with no idea what failed.
    #[test]
    fn redaction_keeps_the_diagnostic_around_the_secret() {
        let got = redact("GET /issues?api_key=abc123&page=2 returned 401");
        assert!(got.contains("GET /issues?"), "{got}");
        assert!(got.contains("&page=2 returned 401"), "{got}");
        assert!(!got.contains("abc123"), "{got}");
    }

    /// Prose must not be mistaken for an assignment. "token: missing" is a
    /// diagnostic, and scrubbing it would destroy the only useful word in it.
    #[test]
    fn prose_after_a_colon_is_not_treated_as_a_credential() {
        assert_eq!(redact("token: missing"), "token: missing");
        assert_eq!(redact("secret: not configured"), "secret: not configured");
    }

    /// Multi-byte input must survive byte-wise scanning. A redactor that panics
    /// or corrupts on a non-ASCII log line would take the logging path down with
    /// whatever it was reporting.
    #[test]
    fn redaction_is_utf8_safe() {
        let body = "sweep ⚠️ failed for проект — 完了 api_key=abc123";
        let got = redact(body);
        assert!(got.contains("⚠️ failed for проект — 完了"), "{got}");
        assert!(!got.contains("abc123"), "{got}");
    }

    /// CORRELATION, the whole point of this feature. A record built inside a call
    /// must carry both ids; one built outside must carry NEITHER — a record with
    /// a trace id and no span id points at a trace and lands nowhere in it.
    #[test]
    fn a_record_carries_both_ids_inside_a_call_and_neither_outside() {
        let inside = log_record(
            &tracing::Level::WARN,
            "think_and_ship::tracker",
            "sweep failed",
            "p",
            Some(&Correlation {
                trace_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                span_id: "bbbbbbbbbbbbbbbb".into(),
            }),
        );
        assert_eq!(inside["traceId"], json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert_eq!(inside["spanId"], json!("bbbbbbbbbbbbbbbb"));

        let outside = log_record(
            &tracing::Level::WARN,
            "think_and_ship::tracker",
            "sweep failed",
            "p",
            None,
        );
        assert!(outside.get("traceId").is_none());
        assert!(outside.get("spanId").is_none());
    }

    /// The severity NUMBER is what a backend's "errors only" filter reads, and
    /// getting it wrong files every error as a warning while the text beside it
    /// still says ERROR — a discrepancy nobody notices until an alert does not
    /// fire.
    #[test]
    fn severity_numbers_match_the_log_data_model() {
        let warn = log_record(&tracing::Level::WARN, "t", "x", "p", None);
        assert_eq!(warn["severityNumber"], json!(13));
        assert_eq!(warn["severityText"], json!("WARN"));
        let err = log_record(&tracing::Level::ERROR, "t", "x", "p", None);
        assert_eq!(err["severityNumber"], json!(17));
        assert_eq!(err["severityText"], json!("ERROR"));
    }

    /// Redaction happens at the ONE seam every body crosses, so no future call
    /// site can forget to ask for it.
    #[test]
    fn the_record_builder_redacts_rather_than_trusting_its_caller() {
        let r = log_record(
            &tracing::Level::WARN,
            "think_and_ship::tracker",
            "auth: Bearer sk-abcdefghijklmnop",
            "p",
            None,
        );
        let body = r["body"]["stringValue"].as_str().expect("body");
        assert!(!body.contains("abcdefghijklmnop"), "{body}");
    }

    /// The envelope must name the same service the SPAN lanes name. If these
    /// drift, a backend files the two signals under different services and the
    /// click-through from a span to its logs — the entire reason for this
    /// module — silently returns nothing.
    #[test]
    fn the_log_envelope_names_the_same_service_as_the_span_lanes() {
        let body = envelope(
            "p",
            vec![log_record(&tracing::Level::WARN, "t", "x", "p", None)],
        );
        let attrs = body["resourceLogs"][0]["resource"]["attributes"]
            .as_array()
            .expect("resource attributes")
            .clone();
        let service = attrs
            .iter()
            .find(|a| a["key"] == "service.name")
            .and_then(|a| a["value"]["stringValue"].as_str())
            .expect("service.name");
        assert_eq!(service, "think-and-ship");
        // The exact shape the OTLP/HTTP logs receiver expects.
        assert!(body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"].is_object());
    }

    /// A full queue must DROP and COUNT, never block — the property that keeps a
    /// wedged collector from wedging the code that logged. Asserted by
    /// overflowing the real channel rather than trusted.
    #[test]
    fn a_full_queue_drops_and_counts_instead_of_blocking() {
        let (tx, rx) = sync_channel(2);
        let dropped = Arc::new(AtomicU64::new(0));
        let inner = Inner {
            tx,
            project: "p".into(),
            dropped: dropped.clone(),
        };
        for _ in 0..10 {
            inner.enqueue(Msg::Record(json!({})));
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 8);
        drop(rx);
    }

    /// The task-local, not a thread-local: a scope must be visible inside it and
    /// gone outside it.
    #[tokio::test]
    async fn the_scope_publishes_the_pair_only_inside_the_call() {
        assert!(current().is_none());
        let c = Correlation {
            trace_id: "t".into(),
            span_id: "s".into(),
        };
        let seen = in_call(Some(c.clone()), async { current() }).await;
        assert_eq!(seen, Some(c));
        assert!(current().is_none(), "the scope must not leak past the call");
        // `None` must still run the future rather than skipping it.
        assert_eq!(in_call(None, async { 7 }).await, 7);
    }
}
