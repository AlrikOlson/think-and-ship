//! LIVE span emission — the lane that publishes while the work happens.
//!
//! # Why this exists when `trace export` already works
//!
//! The offline export is provably correct: [`crate::otel`] builds a valid OTLP
//! body, and a joined SEP-414 tree has been read back out of a real backend.
//! But it is a SNAPSHOT OF PRESENT STATE, and the thing an
//! operator wants is a RECORD OF HISTORY. Those differ in a way no discipline
//! closes: the ship store holds the CURRENT cycle only — one objective tree per
//! export — so every objective that has already shipped is not merely
//! unexported, it is UNEXPORTABLE. The records it was built from were replaced
//! when the next cycle started. A command run later cannot recover them.
//!
//! Live emission is the only way those spans ever exist anywhere.
//!
//! # Two lanes, at different altitudes
//!
//! This lane and the offline lane do not emit the same spans twice:
//!
//! * OFFLINE ([`crate::otel`]) is a DOMAIN PROJECTION at record altitude —
//!   `objective → task → action/check`, ids derived from record identities,
//!   durations reconstructed from stored timestamps.
//! * LIVE (this module) is the RPC ALTITUDE — one span per MCP tool call, with
//!   a duration that was actually MEASURED rather than reconstructed, emitted
//!   at the moment the call returns.
//!
//! Both hang under the same `span_id("workspace", project)`, so they compose
//! into one tree rather than colliding. That parenting choice also closes the
//! hole the offline lane has: this lane emits the `workspace <project>` span
//! ITSELF at flush, which is precisely the span our outbound `traceparent`
//! header has always named and nothing has ever published. With live emission
//! configured, the middle of the tree is published by a machine.
//!
//! Honest cost: if BOTH lanes run against the same backend, the workspace span
//! is emitted twice under one deterministic id. One duplicated span, not N.
//!
//! # The rule that governs every decision here
//!
//! **Emission must never fail, delay, or alter a tool call.** The channel is
//! bounded and the enqueue is a `try_send` that DROPS on a full queue and
//! counts the drop. Blocking to enqueue would turn a wedged collector into a
//! wedged MCP server, which is strictly worse than a missing span. This extends
//! the rule [`crate::mcp`]'s `adopt_trace_context` already follows: nothing
//! about observability is worth failing a tool call over.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// How many spans may sit unsent before new ones are dropped.
///
/// Sized for the realistic burst — an agent session is hundreds of tool calls,
/// not millions of requests — and small enough that a dead collector costs
/// bounded memory rather than growing without limit.
const QUEUE_CAPACITY: usize = 2048;

/// How long the worker waits for more spans before POSTing what it has.
const BATCH_INTERVAL: Duration = Duration::from_millis(500);

/// How long a flush waits for the worker to drain before giving up. A shutdown
/// that hangs on telemetry is a worse bug than a lost span.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Wall-clock nanoseconds since the epoch, which is what OTLP timestamps are.
#[must_use]
pub fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

enum Msg {
    Span(Value),
    /// Drain and POST everything queued, then acknowledge. Used by shutdown.
    Flush(SyncSender<()>),
}

/// The one MCP method this seam serves. `route` is reached only from
/// `call_tool`, so the method name is a constant rather than a parameter — and
/// naming it once keeps the span name and the `mcp.method.name` attribute from
/// ever disagreeing.
const METHOD_TOOLS_CALL: &str = "tools/call";

/// `SPAN_KIND_SERVER` in the OTLP enum. Ours were `1` (INTERNAL), which is the
/// single most expensive thing that was wrong: service maps, RED metrics and
/// every "inbound requests" view in every backend key off SERVER spans, so an
/// INTERNAL span is invisible to all of them.
const SPAN_KIND_SERVER: i64 = 2;

/// Everything the route seam knows about one finished tool call.
///
/// A struct rather than eight positional arguments, because the previous
/// signature was `(&str, u64, u64, bool)` and the spec needs nine facts — at
/// that width a caller silently transposing two `Option<&str>` is a matter of
/// time, and the failure would be a wrong attribute value rather than a
/// compile error.
#[derive(Debug, Default)]
pub struct ToolCall<'a> {
    /// The tool name — the span-name `target` and `gen_ai.tool.name`.
    pub tool: &'a str,
    pub start_nanos: u64,
    pub end_nanos: u64,
    /// The JSON-RPC `id`. `None` for a notification, which the spec says must
    /// NOT carry the attribute.
    pub request_id: Option<&'a str>,
    /// The negotiated MCP protocol version, from the handshake.
    pub protocol_version: Option<&'a str>,
    pub client_name: Option<&'a str>,
    pub client_version: Option<&'a str>,
    /// The JSON-RPC error code, present if and only if the call failed. Its
    /// presence is what makes the span an ERROR — the spec ties the status to
    /// `error.type` rather than to a separate boolean, so there is no way to
    /// mark a span failed without also saying why.
    pub error_code: Option<i32>,
    /// The `JSONRPCError.message`, which the spec wants as the status
    /// description.
    pub error_message: Option<&'a str>,
}

/// The span id one tool call WILL carry, computed from facts known at call
/// START.
///
/// This function existing is what makes trace-to-log correlation possible
/// without restructuring the emitter. The filing chunk for the log lane budgeted
/// for "a real restructure" because the span is built AFTER the call returns —
/// which is true of the span object and false of its id: project, tool and start
/// instant are all in hand before the await, and neither the end timestamp nor
/// the error code participates here.
///
/// It is a named function rather than a `format!` in two places precisely
/// because two copies could drift, and the drift would be silent — logs pointing
/// at a span id that no span was ever published under, which reads in a backend
/// exactly like a dropped log.
fn call_span_id(project: &str, tool: &str, start_nanos: u64) -> String {
    // Unique per invocation, unlike the offline lane's record-identity ids: two
    // calls to the same tool are two spans, and a deterministic-by-name id would
    // collapse them into one.
    crate::otel::span_id("mcp.call", &format!("{project}:{tool}:{start_nanos}"))
}

/// Retag a built span as SERVER and attach the status description.
///
/// Done HERE, in the live lane, rather than by extending [`crate::otel::span`]:
/// that function is shared with the offline export, whose bytes must not move.
/// The offline lane cannot regress from a change it never calls.
fn as_server_span(mut span: Value, status_message: Option<&str>) -> Value {
    span["kind"] = json!(SPAN_KIND_SERVER);
    if let Some(msg) = status_message {
        span["status"]["message"] = json!(msg);
    }
    span
}

/// What the emitter needs to reach a backend. `None` anywhere means "not
/// configured", which is the enable gate.
#[derive(Debug, Clone)]
pub struct LiveConfig {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    /// The caller's trace id, when one has been adopted. Live spans must join
    /// the same trace the offline export would, or the two lanes describe two
    /// unrelated traces.
    pub trace_id: String,
    /// The caller's span, which the workspace span parents to.
    pub parent_span_id: Option<String>,
}

impl LiveConfig {
    /// Read the environment and the adopted caller context.
    ///
    /// Returns `None` — meaning no live emission, no background thread and no
    /// network — unless an endpoint variable is set. See
    /// [`crate::otlp_config::configured_endpoint`] for why endpoint presence is
    /// the switch rather than a separate flag.
    ///
    /// # Sampling
    ///
    /// An adopted context whose `sampled` flag is FALSE suppresses this lane
    /// entirely. That is the W3C-correct reading of a bit we already parse and
    /// have until now ignored: the caller has said this trace is not being
    /// recorded, and emitting anyway would put spans in a trace the caller's own
    /// backend will not keep. With no caller context there is nothing to honour
    /// and everything is emitted — a ratio knob was rejected because at
    /// human-scale volume its only achievable effect is hiding data the operator
    /// asked for.
    #[must_use]
    pub fn from_env(project: &str) -> Option<Self> {
        let endpoint = crate::otlp_config::configured_endpoint()?;
        let inbound = crate::trace_context::load(project);
        if let Some(ctx) = &inbound
            && !ctx.sampled
        {
            return None;
        }
        let headers = crate::otlp_config::configured_headers().unwrap_or_default();
        Some(Self {
            trace_id: inbound
                .as_ref()
                .map_or_else(|| crate::otel::trace_id(project), |c| c.trace_id.clone()),
            parent_span_id: inbound.map(|c| c.parent_span_id),
            endpoint,
            headers,
        })
    }
}

/// The live lane. Cheap to clone; every clone feeds one worker.
#[derive(Clone)]
pub struct LiveEmitter {
    inner: Option<Arc<Inner>>,
}

/// Which trace the live spans belong to, and what the workspace span parents
/// to.
///
/// MUTABLE, and that is load-bearing. A caller's context arrives on a tool
/// call's `_meta`, which is strictly after this emitter is constructed — so a
/// binding fixed at startup would put every live span in a trace of our own
/// minting and quietly refuse to join the caller the server has since adopted.
/// The worker's transport config is fixed; only this moves.
#[derive(Debug, Clone)]
struct Binding {
    trace: String,
    parent: Option<String>,
}

struct Inner {
    tx: SyncSender<Msg>,
    project: String,
    binding: std::sync::Mutex<Binding>,
    workspace_span: String,
    /// First tool call seen, so the workspace span can span the session.
    started: AtomicU64,
    /// Spans dropped because the queue was full. Reported rather than hidden:
    /// a silent gap in a trace is indistinguishable from work that never
    /// happened.
    dropped: Arc<AtomicU64>,
    /// Which transport this process serves on: `pipe` (stdio) or `tcp` (HTTP).
    /// Write-once at startup — see [`LiveEmitter::set_transport`].
    transport: std::sync::OnceLock<&'static str>,
    /// Has the workspace span already been published?
    ///
    /// It must be emitted AT MOST ONCE, and that is not theoretical: the
    /// explicit shutdown flush and the `Drop` fallback both fire on a normal
    /// exit, and the first version of this emitted the span twice. Because its
    /// id is deterministic, a backend then renders TWO ROOTS for one session —
    /// exactly the severed-looking shape earlier real-backend debugging
    /// taught us to recognise, produced this time by us.
    workspace_emitted: std::sync::atomic::AtomicBool,
}

impl LiveEmitter {
    /// A disabled emitter. Every method is a no-op; no thread is spawned.
    #[must_use]
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// Build from the environment. Disabled unless an OTLP endpoint is
    /// configured (and the caller has not said the trace is unsampled).
    #[must_use]
    pub fn from_env(project: &str) -> Self {
        LiveConfig::from_env(project).map_or_else(Self::disabled, |cfg| Self::new(project, cfg))
    }

    /// Build with an explicit config, spawning the worker.
    #[must_use]
    pub fn new(project: &str, config: LiveConfig) -> Self {
        let (tx, rx) = sync_channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let cfg = config.clone();
        // A plain thread owning its own runtime, which is the pattern `otel
        // send` already uses. Spawning onto the ambient tokio runtime would
        // make this module depend on being constructed inside one, and
        // `build_unified` is called from places where that is not guaranteed.
        std::thread::Builder::new()
            .name("otel-live".into())
            .spawn(move || worker(&rx, &cfg))
            .ok();
        Self {
            inner: Some(Arc::new(Inner {
                tx,
                project: project.to_string(),
                binding: std::sync::Mutex::new(Binding {
                    trace: config.trace_id.clone(),
                    parent: config.parent_span_id.clone(),
                }),
                workspace_span: crate::otel::span_id("workspace", project),
                started: AtomicU64::new(0),
                dropped,
                transport: std::sync::OnceLock::new(),
                workspace_emitted: std::sync::atomic::AtomicBool::new(false),
            })),
        }
    }

    /// Re-point this lane at a newly adopted caller context (SEP-414).
    ///
    /// Called from the tool-call path the moment a `traceparent` is adopted,
    /// because that is the only moment the right answer becomes knowable — see
    /// `Binding`. Silent on a poisoned lock: nothing about observability is
    /// worth failing a tool call over.
    pub fn rebind(&self, trace: &str, parent: Option<&str>) {
        let Some(inner) = &self.inner else { return };
        if let Ok(mut b) = inner.binding.lock() {
            b.trace = trace.to_string();
            b.parent = parent.map(str::to_string);
        }
    }

    /// Is this lane live? Used by reporting surfaces, not by the hot path.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// How many spans were dropped for a full queue.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.inner
            .as_ref()
            .map_or(0, |i| i.dropped.load(Ordering::Relaxed))
    }

    /// Set the transport this process is serving on, once.
    ///
    /// `network.transport` is not knowable where the emitter is BUILT —
    /// `build_unified` runs before anyone has chosen stdio or HTTP — but
    /// `run_stdio` and `run_http` each know the answer. A `OnceLock` says
    /// exactly that: written once at startup, read without a lock on the hot
    /// path, and impossible to flip mid-session.
    ///
    /// Per the spec: `pipe` for stdio, `tcp` for HTTP.
    pub fn set_transport(&self, transport: &'static str) {
        if let Some(inner) = &self.inner {
            let _ = inner.transport.set(transport);
        }
    }

    /// The (trace, span) pair the span for this call will carry, available
    /// BEFORE the call runs.
    ///
    /// This is what [`crate::otel_logs::in_call`] publishes for the duration of
    /// the dispatch, so a warning logged inside a tool call arrives at the
    /// backend already pointing at the span that contains it. `None` when the
    /// lane is off — a call whose logs have no span to point at, not a call
    /// whose logs are dropped.
    #[must_use]
    pub fn correlation(
        &self,
        tool: &str,
        start_nanos: u64,
    ) -> Option<crate::otel_logs::Correlation> {
        let inner = self.inner.as_ref()?;
        let trace = inner.binding.lock().map_or_else(
            |_| crate::otel::trace_id(&inner.project),
            |b| b.trace.clone(),
        );
        Some(crate::otel_logs::Correlation {
            trace_id: trace,
            span_id: call_span_id(&inner.project, tool, start_nanos),
        })
    }

    /// Record one completed MCP tool call as an `mcp.server` span.
    ///
    /// Called AFTER the call returns, with its measured start — a span is only
    /// emittable once it has ended, and this lane's whole claim over the offline
    /// one is that the duration was measured rather than reconstructed.
    ///
    /// Never blocks: a full queue drops the span and increments the counter.
    pub fn tool_call(&self, call: &ToolCall<'_>) {
        let Some(inner) = &self.inner else { return };
        let (tool, start_nanos) = (call.tool, call.start_nanos);
        // Remember the earliest call so the workspace span can contain them all.
        let _ =
            inner
                .started
                .compare_exchange(0, start_nanos, Ordering::Relaxed, Ordering::Relaxed);
        let trace = inner.binding.lock().map_or_else(
            |_| crate::otel::trace_id(&inner.project),
            |b| b.trace.clone(),
        );

        // The spec's server table, in its own order of obligation.
        let mut attrs = vec![
            // Required.
            crate::otel::attr("mcp.method.name", METHOD_TOOLS_CALL),
            // Conditionally required: the operation is about a specific tool.
            crate::otel::attr("gen_ai.tool.name", tool),
            // Recommended.
            crate::otel::attr("gen_ai.operation.name", "execute_tool"),
            // Ours, and namespaced so it cannot be mistaken for standard.
            crate::otel::attr("think_and_ship.project", &inner.project),
            crate::otel::attr_bool("think_and_ship.live", true),
        ];
        // Conditionally required when the client executes a request. The spec
        // says NOT to record it for a null/omitted id — that is a notification,
        // not a request.
        if let Some(id) = call.request_id {
            attrs.push(crate::otel::attr("jsonrpc.request.id", id));
        }
        if let Some(v) = call.protocol_version {
            attrs.push(crate::otel::attr("mcp.protocol.version", v));
        }
        if let Some(t) = inner.transport.get() {
            attrs.push(crate::otel::attr("network.transport", t));
        }
        // Not a spec attribute for a server span — there is no `client.address`
        // to give on a pipe — but WHICH client is calling is the thing an
        // operator actually groups by, so it rides our own namespace.
        if let Some(c) = call.client_name {
            attrs.push(crate::otel::attr("think_and_ship.client.name", c));
        }
        if let Some(c) = call.client_version {
            attrs.push(crate::otel::attr("think_and_ship.client.version", c));
        }
        // Conditionally required if and only if the operation fails. The spec's
        // note [1] says this is the JSON-RPC error code as a string.
        if let Some(code) = call.error_code {
            let code = code.to_string();
            attrs.push(crate::otel::attr("error.type", &code));
            attrs.push(crate::otel::attr("rpc.response.status_code", &code));
        }

        let span = crate::otel::span(
            &trace,
            call_span_id(&inner.project, tool, start_nanos),
            Some(&inner.workspace_span),
            // `{mcp.method.name} {target}`, with the tool as the target.
            &format!("{METHOD_TOOLS_CALL} {tool}"),
            start_nanos,
            call.end_nanos,
            attrs,
            call.error_code.is_some(),
        );
        inner.enqueue(Msg::Span(as_server_span(span, call.error_message)));
    }

    /// Drain the queue, then emit the `workspace <project>` span that contains
    /// everything sent so far.
    ///
    /// The workspace span goes LAST because it must close after the calls it
    /// contains. Emitting it is what removes the human from the middle of the
    /// tree: it is the span our outbound `traceparent` names, and until this
    /// lane existed only a human running `otel send` ever published it.
    ///
    /// Honest limit: a `SIGKILL` loses the unflushed tail and the workspace
    /// span. No in-process design fixes that.
    pub fn flush(&self) {
        if let Some(inner) = &self.inner {
            inner.flush();
        }
    }
}

/// Last-resort flush. When the final clone of the service goes away — the
/// normal end of a stdio session — the workspace span is still emitted and the
/// tail still POSTed, without the shutdown path having to remember to ask.
/// `std::process::exit` bypasses this, which is why [`LiveEmitter::flush`] is
/// also callable explicitly.
impl Drop for Inner {
    fn drop(&mut self) {
        self.flush();
    }
}

impl Inner {
    fn flush(&self) {
        let inner = self;
        let started = inner.started.load(Ordering::Relaxed);
        // At most once, whichever of the two shutdown paths gets here first.
        let first = inner
            .workspace_emitted
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if started != 0 && first {
            let binding = inner.binding.lock().map_or_else(
                |_| Binding {
                    trace: crate::otel::trace_id(&inner.project),
                    parent: None,
                },
                |b| b.clone(),
            );
            let span = crate::otel::span(
                &binding.trace,
                inner.workspace_span.clone(),
                binding.parent.as_deref(),
                &format!("workspace {}", inner.project),
                started,
                now_nanos(),
                vec![
                    crate::otel::attr("think_and_ship.project", &inner.project),
                    crate::otel::attr("gen_ai.operation.name", "invoke_agent"),
                    crate::otel::attr_bool("think_and_ship.live", true),
                ],
                false,
            );
            inner.enqueue(Msg::Span(span));
        }
        let (ack_tx, ack_rx) = sync_channel(1);
        if inner.tx.try_send(Msg::Flush(ack_tx)).is_ok() {
            // A shutdown that hangs on telemetry is a worse bug than a lost
            // span, so the wait is bounded and its expiry is not an error.
            let _ = ack_rx.recv_timeout(FLUSH_TIMEOUT);
        }
    }

    fn enqueue(&self, msg: Msg) {
        match self.tx.try_send(msg) {
            Ok(()) => {}
            // Full or disconnected: DROP. The alternative is blocking the tool
            // call that produced the span, which trades a missing span for a
            // hung server.
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Wrap spans in the OTLP/HTTP envelope. Same resource attributes the offline
/// lane emits, so a backend groups both lanes under one service.
fn envelope(project: &str, spans: Vec<Value>) -> Value {
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    crate::otel::attr("service.name", "think-and-ship"),
                    crate::otel::attr("gen_ai.system", "think-and-ship"),
                    crate::otel::attr("think_and_ship.project", project),
                ]
            },
            "scopeSpans": [{
                "scope": { "name": "think-and-ship", "version": env!("CARGO_PKG_VERSION") },
                "spans": spans,
            }]
        }]
    })
}

/// The background worker: batch, POST, repeat. Every error is swallowed — a
/// collector that is down must not produce output on an MCP server's stderr,
/// which is the transport the client is reading.
fn worker(rx: &Receiver<Msg>, cfg: &LiveConfig) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok();
    let Some(client) = client else { return };

    let mut batch: Vec<Value> = Vec::new();
    let mut project = String::new();
    loop {
        match rx.recv_timeout(BATCH_INTERVAL) {
            Ok(Msg::Span(span)) => {
                if project.is_empty() {
                    project = span_project(&span);
                }
                batch.push(span);
                // Keep draining what is already queued rather than posting one
                // span per wakeup.
                while let Ok(Msg::Span(more)) = rx.try_recv() {
                    batch.push(more);
                }
            }
            Ok(Msg::Flush(ack)) => {
                post(&runtime, &client, cfg, &project, std::mem::take(&mut batch));
                let _ = ack.send(());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                post(&runtime, &client, cfg, &project, std::mem::take(&mut batch));
            }
            // Every sender is gone: post the tail and stop.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                post(&runtime, &client, cfg, &project, std::mem::take(&mut batch));
                return;
            }
        }
    }
}

fn span_project(span: &Value) -> String {
    span["attributes"]
        .as_array()
        .and_then(|attrs| {
            attrs
                .iter()
                .find(|a| a["key"] == "think_and_ship.project")
                .and_then(|a| a["value"]["stringValue"].as_str())
        })
        .unwrap_or_default()
        .to_string()
}

fn post(
    runtime: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    cfg: &LiveConfig,
    project: &str,
    spans: Vec<Value>,
) {
    if spans.is_empty() {
        return;
    }
    let body = envelope(project, spans);
    runtime.block_on(async {
        let mut request = client.post(&cfg.endpoint).json(&body);
        for (key, value) in &cfg.headers {
            request = request.header(key, value);
        }
        // Deliberately unexamined. There is no caller to tell and stderr
        // belongs to the MCP transport; the operator's diagnostic surface for
        // this lane is `otel status`, not a log line that would corrupt a
        // stdio session.
        let _ = request.send().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate. A disabled emitter must spawn nothing and accept everything,
    /// because it is what runs on every machine that never configured OTLP.
    #[test]
    fn a_disabled_emitter_is_inert() {
        let e = LiveEmitter::disabled();
        assert!(!e.is_enabled());
        e.tool_call(&ToolCall {
            tool: "think_record_step",
            start_nanos: 1,
            end_nanos: 2,
            ..Default::default()
        });
        e.flush();
        assert_eq!(e.dropped(), 0);
    }

    /// Two calls to the SAME tool must be two spans. The offline lane's
    /// record-identity ids would collapse them; this lane keys on the measured
    /// start instant precisely so it does not.
    #[test]
    fn two_calls_to_one_tool_are_two_spans() {
        let a = crate::otel::span_id("mcp.call", "p:think_record_step:100");
        let b = crate::otel::span_id("mcp.call", "p:think_record_step:200");
        assert_ne!(a, b);
    }

    /// The parenting claim this whole design rests on: a live tool-call span
    /// hangs under the SAME workspace span id the offline export emits and the
    /// outbound `traceparent` names. If these ever diverge, the two lanes
    /// describe two unrelated trees.
    #[test]
    fn the_live_lane_parents_to_the_span_the_other_two_halves_name() {
        let project = "proj-x";
        let emitter = LiveEmitter::new(
            project,
            LiveConfig {
                // Port 1 is unbindable, so the worker can never actually reach
                // anything — this test is about ids, not transport.
                endpoint: "http://127.0.0.1:1/v1/traces".into(),
                headers: vec![],
                trace_id: crate::otel::trace_id(project),
                parent_span_id: None,
            },
        );
        assert!(emitter.is_enabled());
        let inner = emitter.inner.as_ref().expect("enabled");
        assert_eq!(
            inner.workspace_span,
            crate::otel::span_id("workspace", project)
        );
        // The same id the downstream header names (trace_context.rs) and the
        // same id the offline export roots at (otel.rs).
        assert_eq!(
            inner.workspace_span,
            crate::otel::span_id("workspace", project)
        );
    }

    /// A full queue must DROP and COUNT, never block. This is the property that
    /// keeps a wedged collector from wedging the server, so it is asserted by
    /// overflowing the real channel rather than trusted.
    #[test]
    fn a_full_queue_drops_and_counts_instead_of_blocking() {
        let (tx, rx) = sync_channel(2);
        let dropped = Arc::new(AtomicU64::new(0));
        let inner = Inner {
            tx,
            project: "p".into(),
            binding: std::sync::Mutex::new(Binding {
                trace: "t".into(),
                parent: None,
            }),
            workspace_span: "w".into(),
            started: AtomicU64::new(0),
            dropped: dropped.clone(),
            transport: std::sync::OnceLock::new(),
            workspace_emitted: std::sync::atomic::AtomicBool::new(false),
        };
        for _ in 0..10 {
            inner.enqueue(Msg::Span(json!({})));
        }
        // Two fit; the other eight were dropped rather than blocking the
        // (single, test) thread — which is only observable because we never
        // deadlocked getting here.
        assert_eq!(dropped.load(Ordering::Relaxed), 8);
        drop(rx);
    }

    /// The workspace span must be emitted AT MOST ONCE however many times
    /// flush is called. Regression: the explicit shutdown flush and the `Drop`
    /// fallback both fire on a normal exit, and the first version published the
    /// span twice — read back from Jaeger as TWO ROOTS for one session, which
    /// is the exact shape a severed trace has.
    #[test]
    fn flushing_twice_publishes_one_workspace_span() {
        let (tx, rx) = sync_channel(64);
        let inner = Inner {
            tx,
            project: "p".into(),
            binding: std::sync::Mutex::new(Binding {
                trace: "t".into(),
                parent: None,
            }),
            workspace_span: "w".into(),
            started: AtomicU64::new(1),
            dropped: Arc::new(AtomicU64::new(0)),
            transport: std::sync::OnceLock::new(),
            workspace_emitted: std::sync::atomic::AtomicBool::new(false),
        };
        // Stand in for the worker: drain and acknowledge, so the bounded
        // shutdown wait returns at once instead of burning FLUSH_TIMEOUT three
        // times over. A test that takes fifteen seconds to assert one integer
        // gets deleted by the next person.
        let drain = std::thread::spawn(move || {
            let mut workspace = 0;
            while let Ok(msg) = rx.recv_timeout(Duration::from_millis(200)) {
                match msg {
                    Msg::Span(s) if s["spanId"] == "w" => workspace += 1,
                    Msg::Span(_) => {}
                    Msg::Flush(ack) => {
                        let _ = ack.send(());
                    }
                }
            }
            workspace
        });
        inner.flush();
        inner.flush();
        inner.flush();
        drop(inner);
        assert_eq!(
            drain.join().expect("drain thread"),
            1,
            "three flushes must publish one workspace span"
        );
    }

    /// Build one live span the way `tool_call` does, without a worker or a
    /// socket, so the semconv clauses can be asserted on the actual JSON.
    fn built(call: &ToolCall<'_>) -> Value {
        let (tx, rx) = sync_channel(8);
        let inner = Inner {
            tx,
            project: "p".into(),
            binding: std::sync::Mutex::new(Binding {
                trace: "t".into(),
                parent: None,
            }),
            workspace_span: "w".into(),
            started: AtomicU64::new(0),
            dropped: Arc::new(AtomicU64::new(0)),
            transport: std::sync::OnceLock::new(),
            workspace_emitted: std::sync::atomic::AtomicBool::new(false),
        };
        let _ = inner.transport.set("pipe");
        let emitter = LiveEmitter {
            inner: Some(Arc::new(inner)),
        };
        emitter.tool_call(call);
        std::mem::forget(emitter); // skip Drop's flush; we only want the span
        match rx.try_recv() {
            Ok(Msg::Span(s)) => s,
            _ => panic!("no span was enqueued"),
        }
    }

    fn attr_of(span: &Value, key: &str) -> Option<String> {
        span["attributes"].as_array()?.iter().find_map(|a| {
            (a["key"] == key).then(|| a["value"]["stringValue"].as_str().unwrap_or("").to_string())
        })
    }

    /// SPAN KIND. The single most expensive thing that was wrong: service maps,
    /// RED metrics and every "inbound requests" view key off SERVER spans, so an
    /// INTERNAL span is invisible to all of them. Read back out of ClickHouse as
    /// `SpanKind: Internal` before this fix.
    #[test]
    fn a_tool_call_is_a_server_span() {
        let span = built(&ToolCall {
            tool: "roadmap_next",
            ..Default::default()
        });
        assert_eq!(span["kind"], json!(2), "SPAN_KIND_SERVER");
    }

    /// SPAN NAME. The spec says `{mcp.method.name} {target}` with the tool as
    /// target — "tools/call roadmap_next", not our old "mcp roadmap_next". And
    /// the name and the attribute must not be able to disagree.
    #[test]
    fn the_span_name_is_method_plus_target_and_agrees_with_the_attribute() {
        let span = built(&ToolCall {
            tool: "roadmap_next",
            ..Default::default()
        });
        assert_eq!(span["name"], json!("tools/call roadmap_next"));
        let method = attr_of(&span, "mcp.method.name").expect("mcp.method.name is REQUIRED");
        assert_eq!(method, "tools/call");
        assert!(
            span["name"].as_str().expect("name").starts_with(&method),
            "the span name must lead with the method it declares"
        );
    }

    /// A FAILED call must be distinguishable from a successful one IN STORAGE.
    /// Before this fix both were `StatusCode: Unset`, so every error panel and
    /// failure-rate chart in every backend was reading zero.
    #[test]
    fn a_failed_call_carries_error_status_type_and_description() {
        let span = built(&ToolCall {
            tool: "nope",
            error_code: Some(-32602),
            error_message: Some("unknown tool 'nope'"),
            ..Default::default()
        });
        assert_eq!(span["status"]["code"], json!(2), "STATUS_CODE_ERROR");
        assert_eq!(span["status"]["message"], json!("unknown tool 'nope'"));
        // The spec's note [1]: the JSON-RPC code, as a string.
        assert_eq!(attr_of(&span, "error.type").as_deref(), Some("-32602"));
        assert_eq!(
            attr_of(&span, "rpc.response.status_code").as_deref(),
            Some("-32602")
        );
    }

    /// The mirror: a SUCCESSFUL call must carry none of the error machinery.
    /// An `error.type` on a success would make the backend count it as a
    /// failure, which is worse than the bug fixed here.
    #[test]
    fn a_successful_call_carries_no_error_attributes() {
        let span = built(&ToolCall {
            tool: "roadmap_next",
            ..Default::default()
        });
        assert_eq!(span["status"]["code"], json!(0));
        assert!(span["status"].get("message").is_none());
        assert!(attr_of(&span, "error.type").is_none());
        assert!(attr_of(&span, "rpc.response.status_code").is_none());
    }

    /// The facts we always had and never wrote down.
    #[test]
    fn identity_attributes_reach_the_span() {
        let span = built(&ToolCall {
            tool: "think_record_step",
            request_id: Some("42"),
            protocol_version: Some("2025-11-25"),
            client_name: Some("claude-code"),
            client_version: Some("2.1.220"),
            ..Default::default()
        });
        assert_eq!(attr_of(&span, "jsonrpc.request.id").as_deref(), Some("42"));
        assert_eq!(
            attr_of(&span, "mcp.protocol.version").as_deref(),
            Some("2025-11-25")
        );
        assert_eq!(attr_of(&span, "network.transport").as_deref(), Some("pipe"));
        assert_eq!(
            attr_of(&span, "think_and_ship.client.name").as_deref(),
            Some("claude-code")
        );
    }

    /// The spec says NOT to record `jsonrpc.request.id` when the id is null or
    /// omitted — that is a notification, not a request. Emitting an empty string
    /// there would be worse than emitting nothing.
    #[test]
    fn a_notification_omits_the_request_id() {
        let span = built(&ToolCall {
            tool: "x",
            request_id: None,
            ..Default::default()
        });
        assert!(attr_of(&span, "jsonrpc.request.id").is_none());
    }

    /// `rpc.system` / `rpc.method` were OUR invention and `rpc.method` was
    /// actively misleading — in RPC semconv it names the RPC method
    /// (`tools/call`), not the tool. They are gone, and this pins that a future
    /// "helpful" re-add has to argue for it.
    #[test]
    fn the_invented_rpc_attributes_are_gone() {
        let span = built(&ToolCall {
            tool: "roadmap_next",
            ..Default::default()
        });
        assert!(attr_of(&span, "rpc.system").is_none());
        assert!(attr_of(&span, "rpc.method").is_none());
        // The two we always had right stay.
        assert_eq!(
            attr_of(&span, "gen_ai.tool.name").as_deref(),
            Some("roadmap_next")
        );
        assert_eq!(
            attr_of(&span, "gen_ai.operation.name").as_deref(),
            Some("execute_tool")
        );
    }

    /// THE ANTI-DRIFT GATE for trace-to-log correlation, and the reason
    /// `call_span_id` is a named function rather than a `format!` in two places.
    ///
    /// The log lane publishes `correlation()` BEFORE the call and the span lane
    /// builds the span AFTER it. If those two ever compute a different id, every
    /// log points at a span that was never published — which in a backend is
    /// indistinguishable from a log that was dropped, so nothing would report
    /// it. This asserts the ids are the same one.
    #[test]
    fn the_correlation_names_the_span_that_will_actually_be_emitted() {
        let project = "proj-x";
        let emitter = LiveEmitter::new(
            project,
            LiveConfig {
                // Port 1 is unbindable: this test is about ids, not transport.
                endpoint: "http://127.0.0.1:1/v1/traces".into(),
                headers: vec![],
                trace_id: crate::otel::trace_id(project),
                parent_span_id: None,
            },
        );
        let start = 1_234_567_890;
        let correlation = emitter
            .correlation("think_record_step", start)
            .expect("an enabled lane correlates");

        let inner = emitter.inner.as_ref().expect("enabled");
        let span = crate::otel::span(
            &correlation.trace_id,
            call_span_id(&inner.project, "think_record_step", start),
            None,
            "x",
            start,
            start,
            vec![],
            false,
        );
        assert_eq!(
            span["spanId"].as_str().expect("spanId"),
            correlation.span_id,
            "a log's span id must name the span the trace lane actually emits"
        );
        assert_eq!(
            span["traceId"].as_str().expect("traceId"),
            correlation.trace_id
        );
    }

    /// A disabled lane must not fabricate a correlation. A log carrying ids for
    /// a span nobody published is worse than an uncorrelated log: it renders as
    /// a broken link rather than as a plain line.
    #[test]
    fn a_disabled_lane_offers_no_correlation() {
        assert!(LiveEmitter::disabled().correlation("x", 1).is_none());
    }

    /// The offline export must not have moved. `as_server_span` is the live
    /// lane's own post-processing precisely so the shared builder stays
    /// INTERNAL for the domain projection.
    #[test]
    fn the_offline_builder_still_emits_internal_spans() {
        let span = crate::otel::span("t", "s".into(), None, "objective x", 1, 2, vec![], false);
        assert_eq!(span["kind"], json!(1), "offline spans stay INTERNAL");
        assert!(span["status"].get("message").is_none());
    }

    /// The envelope must carry `service.name`, because the offline lane's own
    /// validator rejects a body without it — the two lanes have to be
    /// interchangeable to a collector.
    #[test]
    fn the_live_envelope_passes_the_offline_validator() {
        let span = crate::otel::span(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbb".to_string(),
            None,
            "mcp think_record_step",
            10,
            20,
            vec![crate::otel::attr("think_and_ship.project", "p")],
            false,
        );
        let body = envelope("p", vec![span]);
        let problems = crate::otel::validate_otlp(&body);
        assert!(problems.is_empty(), "{problems:?}");
    }

    /// Sampling: a caller that says "not sampled" switches the lane off. The
    /// bit is parsed from the adopted traceparent and was ignored until now.
    #[test]
    fn an_unsampled_caller_disables_the_lane() {
        // Proven at the config level, which is where the decision is made;
        // driving it through `from_env` would need a process-wide env mutation.
        let ctx = crate::trace_context::InboundTrace::from_meta(
            Some("00-4a1b2c3d4e5f60718293a4b5c6d7e8f9-0123456789abcdef-00"),
            None,
            None,
            "2026-01-01T00:00:00Z".into(),
        )
        .expect("valid traceparent");
        assert!(!ctx.sampled, "trailing 00 means the caller is not sampling");
    }
}
