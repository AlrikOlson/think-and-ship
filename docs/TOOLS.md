# Tool reference

> **Reference** — facts to look up, not a reading path ([all docs](README.md)).

44 canonical tools across four families — `think_*` (reason), `ship_*`
(execute), `roadmap_*` (the long-horizon plan that drives both), and `signal_*`
(what stakeholders asked for).

## `think_*` — the thinking track (11 tools)

Records structured reasoning. The agent writes down *why* before it acts:
steps, branches, revisions, confidence, dependencies, pinned conclusions.

```
think_record_step → think_pin_step → think_trace_checkpoint
```

| Tool                          | Purpose                                              |
|-------------------------------|------------------------------------------------------|
| `think_record_step`           | Record a reasoning step                              |
| `think_revise_estimate`       | Adjust step-count estimate                           |
| `think_pin_step`              | Pin a load-bearing conclusion                        |
| `think_set_branch_status`     | Mark a branch active / merged / abandoned            |
| `think_trace_checkpoint`      | Trace-wide health diagnostics                        |
| `think_get_step`              | Fetch a specific step                                |
| `think_search_trace`          | Search across the trace                              |
| `think_step_impact`           | Blast radius of revising a step                      |
| `think_engine_status`         | Engine introspection                                 |
| `think_export_trace`          | Export in markdown / JSON / console                  |
| `think_wipe_trace`            | Wipe everything (destructive)                        |

## `ship_*` — the doing track (13 tools)

Records structured execution: objectives, task plans, actions, quality
gates, artifacts. The agent tracks *what* it did, *whether it passed*,
and *what it shipped*.

```
ship_set_objective → ship_plan → ship_start → ship_record → ship_check → ship_finalize
```

| Tool                  | Purpose                                                |
|-----------------------|--------------------------------------------------------|
| `ship_set_objective`  | Define goal + acceptance criteria                      |
| `ship_plan`           | Add / remove / reorder tasks                           |
| `ship_start`          | Begin work on a task                                   |
| `ship_record`         | Log an action (code, test, debug, research, review)    |
| `ship_complete`       | Close a task with artifacts                            |
| `ship_block`          | Mark a task blocked                                    |
| `ship_check`          | Record a quality gate (test, lint, build, review); optional `report` {format, path} parses a machine-readable test report (JUnit XML) into structured results next to the exit code |
| `ship_finalize`       | Finalize the objective and emit the ship report        |
| `ship_status`         | Full state snapshot (recovery after context loss)      |
| `ship_export`         | Export the execution trace                             |
| `ship_reset`          | Wipe everything (destructive)                          |

## `roadmap_*` — the planning track (17 tools)

The long-horizon plan-of-plans that sits *above* `ship_*` objectives. A
roadmap is an ordered set of **chunks** (phases); each chunk is realized by
a `ship_*` objective and motivated by `think_*` reasoning, so the three
families fuse into one graph.

```
roadmap_add_chunk → roadmap_start_chunk → (ship objective) → roadmap_complete_chunk
```

`roadmap_export` regenerates a `ROADMAP.md`-shaped markdown view — the
roadmap is native state, the markdown is a generated artifact.
[Roadmap-driven development](WORKFLOWS.md#roadmap-driven-development) walks
the loop.

| Tool                     | Purpose                                                          |
|--------------------------|-----------------------------------------------------------------|
| `roadmap_status`         | Counts + the next-ready chunk + the priority-sorted chunk list  |
| `roadmap_next`           | The most urgent pending chunk (smallest priority number) whose deps are all done        |
| `roadmap_add_chunk`      | Add a chunk (id, title, status, priority, acceptance, deps)     |
| `roadmap_update_chunk`   | Patch a chunk's title / description / acceptance / deps / priority |
| `roadmap_set_status`     | Transition a chunk's status (validated lifecycle)               |
| `roadmap_start_chunk`    | Mark a chunk in_progress; returns the `chunk:<id>` backref      |
| `roadmap_complete_chunk` | Mark a chunk done, attaching a proof-of-ship cross-ref          |
| `roadmap_obsolete_chunk` | Mark a chunk obsoleted with a reason (kept for history)         |
| `roadmap_reprioritize`   | Record a re-prioritization *proposal* (never reorders)          |
| `roadmap_link`           | Attach a `think:`/`task:`/`action:`/`check:`/`chunk:` cross-ref |
| `roadmap_record_refresh` | Record refresh provenance (summary + the think steps behind it) |
| `roadmap_export`         | Markdown / JSON projection — the `ROADMAP.md`-shaped view        |
| `roadmap_get`            | Full records for a named handful of chunks, with sparse fields   |
| `roadmap_set_group`      | Put a chunk in a workstream, or take it out of one               |
| `roadmap_propose_groups` | Which ungrouped chunks belong together, for *you* to name        |
| `roadmap_focus_get`      | What one LANE is working on, and the frontier inside it (read-only) |
| `roadmap_focus_set`      | Point one lane at a workstream and a mode — the only focus writer |

### Focus is per-caller

`roadmap_focus_*` is what the `switch-work` and `advance-work` skills drive.
Focus is `{project, lane, group, mode}`, stored **one record per lane** rather
than one per project: this server is a single process answering every client
that resolves to the same project id, and a repository that declares its
identity gives all of its worktrees that id. A project-global focus would let a
second agent silently re-point the first one's work. There is no such slot to
write into, so that cannot happen — and a lane-less call is refused rather than
collapsed into a shared default, which would be the same bug wearing a
per-caller name.

`group` is the existing [`roadmap_set_group`](#roadmap---the-planning-track-17-tools)
workstream — the one a tracker maps to a project. No second taxonomy exists.
`mode` is closed at exactly `shape` | `build` | `listen`, because each mode
names a *boundary* and an open vocabulary would be a vocabulary of
unenforceable ones.

`roadmap_next` and `roadmap_status` are unchanged and still take no input:
focusing does not narrow what they return. The narrowing lives in the frontier
the focus tools report, which is scoped to one workstream and cannot name a
chunk outside it — so an empty answer means "nothing ready here", never "look
elsewhere".

> **Known gap, priced rather than overlooked.** Both focus tools return
> `structuredContent` but advertise no `outputSchema`. Adding one was measured
> at +1,096 B against 370 B of headroom under the `tools/list` ceiling. The
> shapes are documented in the tool descriptions instead — where a model
> actually reads them, since clients bridging to the Messages API drop
> `outputSchema` before the model sees it. See
> `crates/think-and-ship/src/roadmap/output_schemas`.

## `signal_*` — the stakeholder track (10 tools)

The `signal_*` family tracks **stakeholder signals** — questions, ideas,
concerns, bugs, and feedback raised about the project — and turns the validated
ones into roadmap chunks. It's the inbound half of continuous discovery: a
signal is an *opportunity*; once researched and validated it becomes a roadmap
*solution*, with full provenance back to who raised it.

A signal moves through a validated lifecycle (the engine never lets it move
backward):

```
new → triaged → researched → surfaced → promoted
        └──────────── any non-terminal → dismissed ───────────┘
```

The ten `signal_*` tools, grouped by what they do:

| Group | Tools | Purpose |
|-------|-------|---------|
| capture / read | `signal_capture`, `signal_status`, `signal_get` | record a signal; read the inbox + counts; fetch one |
| churn | `signal_research`, `signal_link` | append a durable enrichment `{ think_step?, sources[], summary, confidence }` and advance to `researched`; attach cross-refs |
| surface | `signal_pending`, `signal_surface`, `signal_snooze`, `signal_ignore` | the ready-to-raise inbox (researched + above-confidence + relevant + not snoozed); raise one; defer; dismiss |
| promote | `signal_promote` | turn a validated signal into a backlog roadmap chunk (writes `chunk:` onto the signal and `signal:` onto the chunk — idempotent) |

Surfacing follows an **earned-interruption** discipline (fewer, higher-confidence
interruptions, never nagging): `signal_pending` returns *only* researched,
above-threshold signals relevant to the active context, so an agent can't raise
a guess. Enrichment is grounded — the reasoning lives in a `think_*` step the
signal cross-refs, not in ephemeral chat.

**Submitting a signal.** Today, signals are captured locally with
`signal_capture` (the agent records stakeholder feedback into the per-project
store at `signal/sessions/<project_id>.json`, beside `think/` and `ship/`).
Direct collaborator submission — webhook, GitHub Issues, inbound email, a public
form — is the **per-tenant cloud backend** (a Cloudflare service; see
[SIGNAL_CONTRACT.md](SIGNAL_CONTRACT.md)), where the local store becomes a
cache of the cloud system-of-record. Inbound email ingress is live (2026-07);
the other submission paths are still on the roadmap.

## Cross-references

The families link to each other automatically:

```
think_record_step:
  execution_ref: "task:auth-refactor"   # points at a ship_* task

ship_record:
  think_step: 19                        # points at think_* step #19
```

All families resolve the same `project_id` from your working directory,
so traces from different conversations in the same project correlate.

## Project identity

Every family keys its store by one `project_id`, resolved in this order:

1. `THINK_AND_SHIP_PROJECT_NAME`.
2. `.think-and-ship/project.json`, found by walking up from the working
   directory the way git finds `.git`.
3. `<dir-basename>-<fnv1a_6hex(canonicalized_cwd)>`.

Without the file, a project *is* its path: rename the directory, move it, or
run a command from `crates/thing` instead of the repo root, and you are a
different project with no history. Declare it once and that stops:

```sh
think-and-ship project mark
```

That writes the id the project **already** resolves to — nothing is minted, so
nothing it holds is detached — and writes nothing else. Commit the file; every
clone then answers the same id at any path. `init` seeds the same file for a
fresh project, alongside the MCP config it writes.

The file carries identity only. It is committed, so a url, profile, token or
tenant in it would be handed to everyone who clones the repository; how *this*
machine reaches a workspace lives in per-machine state instead. `doctor` reports
which of the three sources answered, and warns if a hand-edited id has left the
records on this machine filed under a different one.

## Persistence

Atomic JSON files under one XDG data root, partitioned by family:

```
~/.local/share/think-and-ship/
├── think/sessions/<project_id>.json     # reasoning traces
└── ship/sessions/<project_id>.json      # execution traces
```

## Broadcast

One NDJSON-over-Unix-socket stream with `family` tags so a single viewer
can interleave think + ship events:

```
THINK_AND_SHIP_BROADCAST_PATH=~/.local/share/think-and-ship/broadcast.sock

# Each line:
{ "family": "think", "type": "step_appended", ... }
{ "family": "ship",  "type": "action_recorded", ... }
```

## Environment variables

| Variable                                | Default                                              | Effect                                                  |
|-----------------------------------------|------------------------------------------------------|---------------------------------------------------------|
| `THINK_AND_SHIP_PERSIST`                | `false`                                              | Enable disk persistence                                 |
| `THINK_AND_SHIP_DATA_DIR`               | `~/.local/share/think-and-ship/`                     | Override the XDG data root                              |
| `THINK_AND_SHIP_BROADCAST_PATH`         | _(disabled)_                                         | Unix socket for live broadcast                          |
| `THINK_AND_SHIP_PROJECT_NAME`           | _(from cwd)_                                         | Override project identity                               |
| `THINK_AND_SHIP_AUTO_SESSION`           | `false`                                              | Default session id falls back to the stable `project_id` |
| `THINK_AND_SHIP_DEFAULT_SESSION_ID`     | _(unset)_                                            | Explicit session id override                            |
| `THINK_AND_SHIP_HTTP_ALLOWED_HOSTS`     | `localhost,127.0.0.1,::1`                            | Comma-separated `Host` allowlist for `--http`; replaces the loopback default |
| `THINK_AND_SHIP_HTTP_ALLOWED_ORIGINS`   | _(disabled — `Origin` ignored)_                      | Comma-separated CORS allowlist for browser MCP clients; each entry must include scheme |
| `THINK_AND_SHIP_HTTP_BEARER_TOKENS`     | _(disabled — no auth)_                               | Comma-separated bearer-token allowlist for `--http`; requests need `Authorization: Bearer <token>` or get 401 |
| `THINK_AND_SHIP_SYNC_TARGET`            | `local`                                              | `repo-git` also mirrors traces into `<repo>/.think-and-ship/` (see [SHARED_TRACES.md](SHARED_TRACES.md)) |
| `THINK_AND_SHIP_SHARED`                 | `false`                                              | With `repo-git`, write to the committed `sessions/` partition vs the gitignored `local/` |
| `THINK_AND_SHIP_MODEL_ID`               | _(unset)_                                            | models.dev `provider/model` id used for Agent Trace code attribution |
| `THINK_AND_SHIP_REDACT_PATTERNS`        | _(defaults only)_                                    | Comma-separated extra regexes for the pre-commit redaction hook |
| `THINK_AND_SHIP_CALL_COUNTS`            | `on`                                                 | Set `off` to stop counting tool invocations (see [CLI.md](CLI.md#call-counts)) |
