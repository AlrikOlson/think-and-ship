# Deployment and protocol support

> **How-to** — steps for one task, assuming a working install ([all docs](README.md)).

Which MCP spec revision the server targets, and how to run the Streamable
HTTP transport safely for remote clients — docker, bearer-token auth, host
validation, and CORS.

## Specification compliance

think-and-ship targets **MCP `2025-11-25`** (via rmcp `3.0.0-beta.2`).
Both stdio and Streamable HTTP transports advertise this protocol version
on `initialize`. See [ARCHITECTURE.md](ARCHITECTURE.md) →
*MCP protocol surface* for the extensions that ride the dispatch seam.

### What we don't yet have from `2025-11-25`

The November 2025 interim spec added several capabilities we don't
implement yet — most of them gate on rmcp catching up:

| Capability                       | Status                                 |
|----------------------------------|----------------------------------------|
| Tasks (durable requests, SEP-1686) | Pending rmcp support                 |
| Icons on tools/resources (SEP-973) | Pending rmcp support                 |
| OIDC discovery for auth servers   | Pending rmcp support; we ship unauth   |
| Elicitation redesign (SEP-1330)   | Pending rmcp support                   |
| Tool calling in sampling (SEP-1577) | Pending rmcp support                 |
| JSON Schema 2020-12 default       | ✅ already met (schemars 1.x default)  |

### `2026-07-28` Release Candidate readiness

The RC is a breaking spec revision: stateless transport (no
`Mcp-Session-Id`, no `initialize` handshake), `_meta`-envelope routing,
multi-round-trip requests, hardened OAuth, an Extensions framework, and
the `-32002` → `-32602` error-code flip for missing resources.

**Existing v0.2.0 deployments do not break.** SEP-2596 guarantees a
**≥12-month deprecation window** between a spec being marked deprecated
and being removed, so a `2025-06-18` server stays valid against any
`2026-07-28`-aware client for at least a year after the new spec ships.

When [rust-sdk#526](https://github.com/modelcontextprotocol/rust-sdk/issues/526)
lands (SEP-1442 statelessness), the migration on our side is
expected to be one wiring change in `cli/mod.rs` — the application-level
session id (which keys persistence and broadcast files) is independent
of the protocol session and continues unchanged.

## Remote deployment

The Streamable HTTP transport is meant for remote MCP clients (browser
extensions, hosted agents, edge workers). Two env vars gate it for
public-facing use; both default to safe loopback-only behavior.

### Docker quickstart

```sh
docker build -f docs/deploy/Dockerfile -t think-and-ship:0.2.0 .
docker run --rm -p 8080:8080 -v ts-data:/data think-and-ship:0.2.0
# → think-and-ship http on http://0.0.0.0:8080/mcp
```

The image is a multi-stage `rust:1.88-slim` → `debian:bookworm-slim`
build with a non-root `think` user and persistence on by default to
`/data`. See [`deploy/Dockerfile`](deploy/Dockerfile) for the
full build and verification commands.

### Authentication (bearer tokens)

By default the `--http` listener is **unauthenticated** — host/CORS validation
guard against DNS-rebinding, not against unauthorized callers. For any
non-loopback deployment, require a bearer token:

```sh
THINK_AND_SHIP_HTTP_BEARER_TOKENS=tok_alice,tok_bob   # comma-separated allowlist
```

When set, every request needs `Authorization: Bearer <token>` with a token from
the list; anything else gets `401 Unauthorized` (with `WWW-Authenticate: Bearer`)
before reaching the MCP handler. Unset → no auth layer (the listener stays open).
Point clients at the server with the header configured in their MCP transport.

> This is transport-level shared-secret auth, suitable behind your own TLS /
> reverse proxy. Full OAuth (rmcp's `auth` feature) lands with the
> `2026-07-28` spec work — see the roadmap.

### Host validation (DNS-rebinding protection)

By default the server only accepts requests whose `Host` header is
`localhost`, `127.0.0.1`, or `::1` — the rmcp transport ships this
protection against DNS-rebinding attacks against locally running MCP
servers. Public deployments override the list with their own hostnames:

```sh
THINK_AND_SHIP_HTTP_ALLOWED_HOSTS=mcp.example.com,mcp.example.com:8080
```

> ⚠️ The list **replaces** the default — if you want browsers on the
> same machine to still hit `http://localhost:8080/mcp`, include
> `localhost,127.0.0.1` explicitly:
> `THINK_AND_SHIP_HTTP_ALLOWED_HOSTS=mcp.example.com,localhost,127.0.0.1`

### CORS (browser MCP clients)

Origin validation is **disabled** by default (the rmcp transport ignores
the `Origin` header when the allowlist is empty), which is the right call
for non-browser clients. Browser-based MCP clients send `Origin`, so you
need to enumerate the ones you trust:

```sh
THINK_AND_SHIP_HTTP_ALLOWED_ORIGINS=https://app.example.com,http://localhost:5173
```

Entries must include the scheme. Requests carrying an `Origin` that
isn't on the list are rejected; requests with no `Origin` (e.g. `curl`,
non-browser SDKs) still pass.

The server logs both lists at startup so you can confirm what was
picked up:

```
http allowed hosts: ["mcp.example.com", "localhost", "127.0.0.1"]
http allowed origins: ["https://app.example.com"]
think-and-ship http on http://0.0.0.0:8080/mcp
```
