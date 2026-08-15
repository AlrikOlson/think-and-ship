# OpenTelemetry export

> **How-to** — steps for one task, assuming a working install ([all docs](README.md)).

`trace export` writes the workspace as one OpenTelemetry trace, and the MCP
server can emit live spans per tool call. This document covers both lanes,
credentials for hosted intakes, and joining the caller's trace (SEP-414).

Agent observability converged on the [OTel GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/),
and think-and-ship speaks them natively: `trace export` maps your
workspace onto agent spans — the ship cycle as an `objective → task →
action/check` span tree (failed gates become ERROR-status spans), think steps
as reasoning spans parented by their `execution_ref`. Trace and span ids are
sha256-derived from record identities, so the same corpus always exports the
same trace.

Guided setup — no docker or curl typing:

```sh
think-and-ship otel wizard
```

It writes a docker-compose for a local Jaeger, starts it, waits for the collector to
actually listen, exports this project's trace and POSTs it, then hands you the UI link.
Without a terminal it prints the plan and changes nothing, so it can never hang CI or an
agent; `--yes` runs it unattended.

Every step is also a standalone command, because a wizard you cannot script is worse than
the commands it replaced:

```sh
think-and-ship otel up       # generate the stack + start it (idempotent)
think-and-ship otel send     # export + POST, reports what the collector said
think-and-ship otel status   # docker? stack? endpoint? anything to send?
think-and-ship otel down     # stop it
```

`trace export` remains the pure, pipeable primitive if you would rather POST the body
yourself, or send it somewhere that is not local:

```sh
think-and-ship trace export --out trace.json
curl -X POST -H 'Content-Type: application/json' -d @trace.json http://localhost:4318/v1/traces
```

The same file POSTs to any OTLP/HTTP endpoint (an OTel collector, Grafana
Tempo, Datadog's OTLP intake). Honest limits: the local ship store holds the
current cycle only, so the export carries one objective tree per run; think
steps without timestamps are skipped and counted, never fabricated.

**Credentials for a hosted intake.** Every OTLP backend worth sending to wants
a header, and `otel send --endpoint` reads it from the environment rather than
from a flag: set `OTEL_EXPORTER_OTLP_HEADERS` to a comma-separated `key=value`
list — `x-honeycomb-team=<key>` for Honeycomb, `DD-API-KEY=<key>` for Datadog,
`Authorization=Basic <base64 instance:token>` for Grafana Cloud. That is the
variable the OpenTelemetry exporter specification already defines, so it is the
one your vendor's own onboarding page tells you to set; values are
percent-decoded per the spec's W3C Baggage form, which is how a Basic
credential's space survives. `OTEL_EXPORTER_OTLP_TRACES_HEADERS` overrides it
for traces specifically, and *replaces* rather than merges, per the same spec.
There is deliberately **no `--header` flag**: an OTLP credential is an API key
by definition, and arguments are readable by any process through `ps` and
persist in shell history — so the flag would only be the unsafe way to say the
same thing. `otel status` reports which header names it found, never their
values. A 401 or 403 from an intake says so in those terms and tells you which
of the two situations you are in — no credential sent, or one sent and
rejected.

**`otel status` tells you which of these you are in.** The dangerous state is
*context adopted, nothing exported*: from the moment a caller's `traceparent` is
adopted, our outbound requests name a workspace span that only exists once an
export lands, and until then the caller's tree shows those legs as a **separate
root** rather than under us. Nothing errors — the collector answers 200, the UI
renders, and the only diagnostic anywhere is a per-span clock-skew warning that
is about timing math, not about the severed tree. So `otel status` says so in
those terms, and stops saying it once an export has landed. It also prints an
`exported:` line — when, where, and how many spans — because "never sent
anywhere" is the most useful thing that line can say.

Relatedly, `otel send` warns before re-sending to an endpoint that already has
this trace. It does not refuse: re-sending after recording more work is the
normal thing to do. But span ids are deterministic and OTLP has no upsert, so
the collector *appends* — the old copies stay, and the backend ends up holding
each span twice.

## Live emission — the second lane

Everything above is the OFFLINE lane: a command a human runs, producing a
deterministic projection of the records as they stand right now. It has a limit
no discipline fixes — the ship store holds the CURRENT cycle only, so an
objective that has already shipped is not merely unexported but *unexportable*.
A snapshot cannot be a history.

So there is a second lane. Set `OTEL_EXPORTER_OTLP_ENDPOINT` (a base URL, with
the `/v1/traces` path appended per the exporter spec) or
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (used exactly as given, no path appended)
and the MCP server emits one span per tool call, as the calls happen, to that
endpoint — with the credential headers described above. Presence of the endpoint
*is* the switch: with neither variable set the server starts no thread and makes
no network call, which is the default.

The two lanes sit at different altitudes and do not duplicate each other. The
offline lane is a domain projection — `objective → task → action/check`, ids
derived from record identities, durations reconstructed from stored timestamps.
The live lane is the RPC layer, and it speaks OpenTelemetry's **MCP semantic
convention**: an `mcp.server` span per call, named `tools/call <tool_name>`,
`SpanKind: SERVER`, with `mcp.method.name`, `mcp.protocol.version`,
`jsonrpc.request.id`, `network.transport` (`pipe` for stdio, `tcp` for HTTP) and
`gen_ai.tool.name`. A failed call carries `error.type` set to the JSON-RPC code,
`rpc.response.status_code`, an `ERROR` status and the error message as the
status description. Both lanes hang under the same `workspace <project>` span,
so they compose into one tree.

That conformance is what lets a backend do more than list spans. Because the
spans are `SERVER` kind and failures are stored as `ERROR` rather than `Unset`,
per-tool call counts, error rates and latency come out of a stock RED query with
no custom fields — and the service map has something to draw.

`mcp.session.id` is deliberately absent on stdio: MCP session management is a
property of the HTTP transport, and minting a per-process id would put a value
in a standard field meaning something different from what every other emitter
puts there. Tool arguments and results (`gen_ai.tool.call.arguments` /
`.result`) are Opt-In in the spec and are **not** captured — on this project a
tool argument is a reasoning step's full text, so that needs its own consent
decision rather than a default.

That parenting is also what closes the gap described below: the live lane
publishes the `workspace <project>` span itself when the session ends, and that
is precisely the span our outbound `traceparent` header names. With live
emission configured, no human has to run anything for the middle of the tree to
exist. Honest limits: a `SIGKILL` loses the unflushed tail and the workspace
span; a full queue drops spans rather than delaying the tool call that produced
them; and if you run both lanes against one backend the `workspace` span arrives
from each.

## Joining the caller's trace (SEP-414)

An export like the above is an *island*: its trace id is ours, so it can
never appear inside the span tree of the host that called you. MCP's
SEP-414 fixes that by carrying [W3C Trace
Context](https://www.w3.org/TR/trace-context/) in a request's `_meta`
under the reserved `traceparent` / `tracestate` / `baggage` keys.

When a client sends a `traceparent` on a tool call, the server adopts it
and the next `trace export` **joins that trace** instead of minting
its own — every span carries the caller's trace id, and our root span
parents to the caller's span. POST that body to the backend your host
already writes to and you get one span tree spanning host → client →
think-and-ship, rather than two unrelated roots.

```
joined caller trace 0af7651916cd43dd8448eb211c80319c (root parents to span 00f067aa0ba902b7, adopted 2026-07-27T18:20:01Z)
```

Details worth knowing:

- **Opt-in with everything else.** Adoption persists under
  `THINK_AND_SHIP_PERSIST` — the server adopts, and `trace export` reads it
  back in a later process. With persistence off, nothing is adopted.
- **Span ids do not change.** Only the trace id and the root's parent do.
  The sha256-derived span ids stay, so an unchanged corpus still exports
  the same structure.
- **No context means no change.** With nothing adopted, the export is
  byte-identical to what it was before SEP-414 existed.
- **Bad context is ignored, never fatal.** A `traceparent` that violates
  the W3C rules — uppercase hex, an all-zero trace-id or parent-id, the
  reserved `ff` version — is treated as absent, exactly as the spec
  instructs. A tool call is never failed over trace metadata.
- **This is offline join, not live emission.** think-and-ship does not run
  an OpenTelemetry SDK pipeline and emits no spans in real time; it makes
  its exported trace land in the right place in yours.
- **The tree has a hole in it until you export.** This is the one failure
  mode worth planning for, because the middle of the tree is published by a
  human running a command while both ends are published by machines in real
  time. Adopt a context, let a tracker push go out — its `traceparent` names
  our workspace span as its parent — then never run the export, and the
  backend has no such span to hang it from. Verified against Jaeger: the
  downstream leg detaches into a **second root**, so the trace renders as two
  disconnected fragments rather than one chain. The mirror case, where the
  export lands but the caller's own span never does, is quieter and worse to
  diagnose: it renders as a single, ordinary-looking trace rooted at
  `workspace <project>`, with nothing to suggest it was meant to hang under
  someone. Neither case is an error anywhere — the collector answers 200 and
  the UI renders. The only tell is a per-span warning, and it is about clock
  skew, not about the broken link. If you want the joined tree, export.

The host → think-and-ship → downstream chain above is observed rather than
asserted: an adopted `traceparent`, a real export, and a real outbound header
were POSTed to a local Jaeger and read back through its query API as one
trace — a single root, three services, and the downstream leg nested under
`workspace <project>` rather than beside it.
