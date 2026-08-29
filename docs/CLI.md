# CLI reference

> **Reference** — facts to look up, not a reading path ([all docs](README.md)).

Every command the `think-and-ship` binary accepts, plus the config `init`
writes and the local call-count store.

## Configure

`think-and-ship init` auto-detects your IDE and writes the config:

| IDE         | Config file        | Detection                  |
|-------------|--------------------|----------------------------|
| Claude Code | `.mcp.json`        | default                    |
| Cursor      | `.cursor/mcp.json` | `.cursor/` dir exists      |
| Windsurf    | `.windsurf/mcp.json` | `.windsurf/` dir exists  |

The generated config — **one entry**, not two:

```json
{
  "mcpServers": {
    "think-and-ship": {
      "command": "think-and-ship",
      "args": ["serve"],
      "env": { "THINK_AND_SHIP_PERSIST": "true" }
    }
  }
}
```

### `connect` searches instead of detecting

`init` is *authoring* an entry, so the table above is a reasonable guess. `connect`
is *updating* one, and there a guess writes a working cloud credential into a file
your agent never opens — you are told "Connected" while the agent stays local. So
`connect` searches every place an entry can live and updates the one that is
actually there:

| Host        | Config file          | Servers under |
|-------------|----------------------|---------------|
| Claude Code | `.mcp.json`          | `mcpServers`  |
| Cursor      | `.cursor/mcp.json`   | `mcpServers`  |
| Windsurf    | `.windsurf/mcp.json` | `mcpServers`  |
| VS Code     | `.vscode/mcp.json`   | `servers`     |
| Claude Code | `~/.claude.json`     | `mcpServers`, under this project's path — where `claude mcp add` writes |

VS Code is searched but never authored into: a `.vscode/` directory exists in
plenty of repositories that never use agent mode, so it is not a signal about
where a new entry belongs.

Three outcomes, each said out loud rather than guessed past:

- **One entry found** — it is updated in place, wherever it lives.
- **No entry yet** — `connect` says it is creating one, and authors it per the
  `init` table.
- **Several entries found** — `connect` stops and names them. Writing to one would
  leave the others stale, and which one your agent reads is the host's decision.

`~/.claude.json` is the one config `connect` will not rewrite: it holds every
project you have, and round-tripping it reorders the whole file. When your entry
lives there, `connect` prints the `claude mcp remove … && claude mcp add-json …
--scope local` command that edits it safely, with the entry already filled in.
(Remove first, because `add-json` refuses an existing name — and the entry
provably exists there, or `connect` would not be printing the command.)

### Connect ends in a fact

Before `connect` says "Connected", it makes one authenticated request against
the backend with the token it just stored — resolved back out of the credential
store, the same way the server resolves it at startup. A rejected credential
fails the command instead of reporting success, and leaves the MCP config
untouched so a retry starts clean.

The closing message names the one client whose config was written and that
client's actual reload step (Claude Code: `/mcp`; Cursor: Settings → Tools &
MCP; Windsurf: the Cascade plugins panel; VS Code: "MCP: List Servers") — not a
list of every client that exists.

### The token is not in the config

`connect` writes a profile **name**, not a secret. The MCP entry gets
`THINK_AND_SHIP_CLOUD_PROFILE=<name>`; the token itself goes into this machine's
credential store, and the server resolves the name at startup.

This is not cosmetic. An MCP config is a file that gets committed
(`.mcp.json`, `.cursor/mcp.json` sit in your repo root), synced out of a home
directory, and pasted into support threads. A bearer token good for months does
not belong in one.

Where the token actually lands, in order of preference:

| Store | When | What it is |
|---|---|---|
| OS keychain | macOS and freedesktop Linux, when a keyring answers | `security(1)` / `secret-tool(1)`, driven as a subprocess — no library is linked |
| Encrypted file | Everywhere else: headless containers, Windows | ChaCha20-Poly1305 under your data dir. See the credential store's own docs for what that does and does not defend against |

`THINK_AND_SHIP_CLOUD_TOKEN` still works and is still documented — CI has no
keychain and no interactive login, and deployment runbooks use it. When
set, it **wins** over a stored profile: an operator who puts a token in the
environment has said something explicit.

If you connected before this existed, your next `connect` moves the plaintext
token out of the config and into the store. That happens without `--force`, and
before the browser step, so an abandoned sign-in still leaves the secret in the
right place. When the token is in `~/.claude.json` — which this tool will not
rewrite — it is adopted anyway and you are told which line to delete.

`think-and-ship disconnect` reverses both halves at once: it forgets the stored
token *and* strips the cloud settings from the entry. Doing only one would leave
you either with a server that tries to sync and fails, or with a live long-lived
credential on a machine you believe is disconnected.

## Commands

| Command | What it does |
|---|---|
| `think-and-ship serve` | Run as an MCP server on stdio |
| `think-and-ship serve --http :8080` | Run as an MCP server over Streamable HTTP |
| `think-and-ship init` | Write MCP config for your IDE |
| `think-and-ship init --full` | MCP config + CLAUDE.md tool reference |
| `think-and-ship init --dry-run` / `--force` | Preview without writing / overwrite existing config |
| `think-and-ship project mark [--name NAME] [--dry-run]` | Declare this repository's identity in `.think-and-ship/project.json` — and write nothing else |
| `think-and-ship skills install` | Install the core skills (`switch-work`, `advance-work`) for every detected agent |
| `think-and-ship skills install --client all --scope project` | All twelve agents; this repository rather than your home directory |
| `think-and-ship skills install --profile legacy` | The pre-Iteration-3 skills, which no longer install by default |
| `think-and-ship skills list [--scope user\|project]` | Bundled skills with their profile, and each agent's tier, destination and install state |
| `think-and-ship skills package --client claude-code\|codex --out DIR` | Build that agent's own plugin from the canonical source. Builds only; publishes nothing |
| `think-and-ship skills migrate [--apply] [--force]` | Retire destinations this installer no longer writes. Previews by default; removes only provably unchanged copies |
| `think-and-ship roadmap next` | The next ready chunk (deps done, most urgent = smallest priority number) |
| `think-and-ship roadmap status` | The plan at a glance: counts by status + what's next |
| `think-and-ship roadmap export [--format markdown\|json]` | Render the roadmap as a `ROADMAP.md`-shaped view |
| `think-and-ship roadmap import --file ROADMAP.md [--merge] [--dry-run]` | Seed roadmap chunks from a hand-written roadmap |
| `think-and-ship roadmap hygiene [--dry-run]` | Flag stalled / ready-but-idle chunks as signals |
| `think-and-ship roadmap regions [--file MAP] [--apply]` | Audit the region map — the places the plan is navigated by — or re-author it from a JSON map of region name to chunk ids |
| `think-and-ship trace export [--out FILE]` | Export the trace as OTLP/JSON with OTel GenAI spans (see [OBSERVABILITY.md](OBSERVABILITY.md)) |
| `think-and-ship trace promote --session <id> [--step N]` | Promote private trace records to the shared partition (see [SHARED_TRACES.md](SHARED_TRACES.md)) |
| `think-and-ship corpus export [--out FILE]` | Versioned structural event corpus. Reads only this workspace's local stores; the file stays local |
| `think-and-ship corpus eval [--as-you-go\|--learned]` | Replay the corpus and score next-chunk predictors (the two modes are mutually exclusive) |
| `think-and-ship sync push [--dry-run]` | One-shot back-fill of the local corpus to the cloud |
| `think-and-ship calls [--json]` | Per-tool invocation counts for this project — local only, never transmitted (see [Call counts](#call-counts)) |
| `think-and-ship telemetry status\|on\|off\|push` | Anonymized-telemetry consent (off by default) |
| `think-and-ship connect [--url URL]` | Device-flow login; stores the token in the credential store and writes the profile name to the MCP config |
| `think-and-ship disconnect [--dry-run]` | Forget the stored token and strip the cloud settings from the MCP entry |
| `think-and-ship doctor` | Diagnose setup issues |
| `think-and-ship status` | Project info + config state |
| `think-and-ship repair [--dry-run]` | Repair the think trace (duplicate step clones) |
| `think-and-ship --version` | Show version info |

**A group is a noun, a command is a verb.** Areas — `roadmap`, `trace`,
`corpus`, `skills`, `sync`, `telemetry`, `project` — group the verbs that act
on them; only commands that act on the installation itself stay at the top
level.
`export`, `import`, `hygiene`, `promote`, and `eval` were top-level through
v0.3.x: they still work, print a one-line note pointing at the new spelling,
and are hidden from `--help`.

The `--http` flag accepts `host:port`, `:port`, or a bare `port` (defaults
to `127.0.0.1`). The MCP endpoint is mounted at `/mcp`:

```sh
think-and-ship serve --http :8080
# → think-and-ship http on http://127.0.0.1:8080/mcp
```

## Call counts

`think-and-ship calls` shows how many times each tool has been dispatched on
this machine, for this project:

```
tool calls for think-and-ship-6353e7
  last call: 2026-07-28T02:13:47+00:00

  roadmap_status              3
  think_engine_status         1
  <unrecognized>              1

  TOTAL                       5  (3 distinct)

  soak NOT MET (5 calls over 1 active days). A zero is NOT yet evidence a verb
  is unused — it may just be a workflow nobody has run on this build.
    still needs: 495 more call(s) — 5 of 500
    still needs: 13 more active day(s) — 1 of 14
```

The dispatcher increments the count; nothing asks the agent what it used. That
distinction is the whole point — the `tools_used` field on a think step is a
self-report, and across 191 session files it recorded `think_record_step` **3**
times against **9,999** persisted steps. Treat it as prose, never as evidence.

Every `tools/call` counts, **including calls that fail** and calls a narrowed
deployment refuses: a verb nobody can use correctly must not read as a verb
nobody uses. Unrecognized names collapse into one `<unrecognized>` bucket
rather than minting a key each.

**These counts are not telemetry, and they are on by default.** The opt-in
posture that governs `telemetry` is about *egress* — your content leaving your
machine. A count is not content: the key space is the closed set of tool names
this binary registers, and the value is an integer. It is also not a new
disclosure, since the 9,999 think steps in the same data dir already record
those calls; counting just makes the read cheap and covers the read-only verbs
that leave no artifact. And it cannot be sent anywhere — not by promise, but
because no code path exists: the counter lives in its own `usage/` store
partition that the telemetry extractor does not read, and no telemetry module
references it.

Counting writes to the same store as everything else, so it is already off
wherever `THINK_AND_SHIP_PERSIST` is. To turn it off on its own, set
`THINK_AND_SHIP_CALL_COUNTS=off`.

### When a zero means something — the soak window

A count of 0 has two causes, and the number cannot tell them apart: the verb is
genuinely unused, or **nobody has run the workflow that uses it yet**. On a
fresh install every one of the tools reads 0 and none of them are cold.

So `calls` will not let you read a zero as a verdict until the counter has
soaked. Ask about one verb directly:

```
$ think-and-ship calls signal_research
signal_research: 0 calls — NOT EVIDENCE. The soak window is not met
(5 calls over 1 active days); still needs 495 more call(s) — 5 of 500;
13 more active day(s) — 1 of 14.
```

The threshold is **500 calls and 14 active days — both, not either**. One
session dispatches 40–80 calls, so 500 is an order of magnitude past "one
session's path" and no single workflow can dominate. Fourteen *active* days —
days on which a call actually landed — reach the weekly-cadence workflows that a
one-to-three-day window structurally cannot.

Active days rather than calendar days is a deliberate departure from the nearest
industry convention (Azure API Management calls an endpoint unused after 30 days
without traffic). That rule assumes an always-on service, where calendar time is
a fair proxy for opportunity. This binary only runs while a session runs, so
thirty quiet days may be thirty days of never being asked — and a silence nobody
had the chance to break is not evidence.

Even a met soak is qualified per verb: if a verb reads 0 **and its whole family
reads 0**, the answer is "that workflow was never run", not "cold". Only a
`COLD` verdict — met window, exercised family, still zero — supports retiring
anything.
