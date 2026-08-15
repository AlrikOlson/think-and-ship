# Changelog

All notable changes to think-and-ship are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project uses
SemVer.

## [Unreleased]

## [0.4.0] - 2026-08-15

### Changed (breaking)

- **`connect` no longer has a built-in backend.** The cloud URL now comes from
  `--url` or the `TAS_CLOUD_URL` environment variable, in that order, and
  the command fails naming both when neither is set. Previously it defaulted to
  a hosted endpoint. The host `connect` resolves is printed to the user *and*
  written into every MCP config it authors, so it outlives the session in a file
  you keep — choosing it is the operator's call, and a self-hosted deployment is
  now named exactly the way a hosted one is. Existing MCP configs are unaffected;
  only a fresh `connect` needs the value.

  ```sh
  # before
  think-and-ship connect
  # after
  TAS_CLOUD_URL=https://api.example.com think-and-ship connect
  # or
  think-and-ship connect --url https://api.example.com
  ```

  Resolution is validated rather than assumed: a trailing slash and surrounding
  whitespace are absorbed, an empty value is refused as a misconfiguration, and
  cleartext `http` is refused anywhere but loopback, since a bearer token is
  exchanged over this origin.

- **Telemetry has no built-in ingest endpoint.** `THINK_AND_SHIP_TELEMETRY_URL`
  is now the only source. Unset, or set empty, means there is nowhere to send:
  telemetry is structurally zero regardless of consent state. Collection was
  already consent-gated and opt-in; this removes the default destination, so an
  operator who wants shapes collected names the endpoint that receives them.

### Added

- **Two core skills replace the sprawling default surface** — **`switch-work`** picks the workstream
  and mode you are working in, **`advance-work`** does exactly one evidenced unit there and stops.
  `switch-work` changes no code, no chunk and no ordering; an unknown or ambiguous workstream writes
  nothing and hands back the real candidates. `advance-work` honours three modes: `shape` completes
  one planning or research decision and modifies no implementation source, `build` completes one
  ready chunk behind the project's real gates and refuses completion while a required check is red,
  skipped or unverified, and `listen` processes one signal and never a second. Every invocation ends
  with a receipt naming the unit, the evidence, the native records and the next candidate — which is
  recomputed, never executed. Spec Kit is an automatic adapter, not a separate skill: it engages when
  `.specify/` exists and fabricates nothing when it does not.
- **Focus is native and per-caller** — `roadmap_focus_get` (read-only) and `roadmap_focus_set` (the
  only writer) hold `{project, lane, group, mode}`. Stored one record per **lane** rather than one
  per project, so two worktrees or two concurrent agents cannot overwrite one another: there is no
  project-wide slot to clobber. A lane-less call is refused with a recipe for producing one rather
  than defaulting to a shared key, because that default *is* the clobber. `group` reuses the existing
  roadmap workstream — no second taxonomy — and `mode` is closed at exactly three. Focus persists
  when persistence is enabled, survives restart, and a store written before focus existed loads
  unchanged. `roadmap_next` and `roadmap_status` keep their no-input behaviour exactly; compact chunk
  summaries now carry `group`.
- **Twelve coding agents, two scopes, three profiles** — the three-client installer became a
  capability-driven harness model covering Claude Code, Codex, GitHub Copilot, Cursor, Gemini CLI,
  Windsurf, OpenCode, Cline, Roo Code, Amp, Goose and Kiro. `--scope user|project`,
  `--profile core|legacy|all`, and every destination recorded in the new **`docs/HARNESSES.md`**
  against the first-party page it was read from, dated 2026-08-08. `skills list` shows profile, tier,
  destination, distribution and install state. `skills package` builds Claude Code and Codex plugins
  from the same render the installer writes (generated, never stored — a committed package would be a
  second copy of both skills); a harness documenting no plugin format is refused rather than given an
  invented one. Windsurf additionally gets a thin manual-only workflow that points at the skill,
  because its own guidance makes Workflows the manual-only primitive where Skills are not.
- **`think-and-ship skills migrate`** — retires destinations this installer no longer writes.
  Previews by default; removes only a directory it can prove is byte-identical to this binary's own
  render. Anything else is reported and kept, because a local edit and an older version's copy are
  indistinguishable from the filesystem.

### Fixed

- **Codex's skills destination was wrong.** The installer wrote `~/.codex/skills`, a path asserted
  from a module comment that no first-party OpenAI page lists. Codex reads `~/.agents/skills`. This
  was not merely untidy: Cursor's documented legacy compatibility list includes the old path, so a
  stale copy there was still discovered and could answer instead of the current skill.
- **Four bundled skills failed the official Agent Skills validator** and had done for some time —
  `business-intel` exceeded the 1024-character description limit, while `craft`, `gui-scrutiny` and
  `roadmap-run` had invalid frontmatter YAML because an inline description contained an unquoted
  colon. All thirteen bundled skills now validate.
- **Stale tool counts in six places.** `51` was asserted in four test files and a module header while
  the registry served 53, and `docs/TOOLS.md` claimed 12 roadmap and 11 ship tools against a real 17
  and 13. The per-family counts in `docs/TOOLS.md` are now checked against the live registry by a
  test rather than maintained by hand.

- **Three more bundled skills**, so `think-and-ship skills install` carries the whole `/roadmap`
  family rather than two of it: **`/roadmap-run`** (advance several chunks as one
  dependency-ordered run, re-deriving the frontier between them so work that has been subsumed is
  noticed), **`/roadmap-sk`** and **`/roadmap-refresh-sk`** (the `/roadmap` and `/roadmap-refresh`
  loops for a [Spec Kit](https://github.com/github/spec-kit) project). `/roadmap-sk` was already in
  use as a hand-installed skill and had never been part of the bundle; `/roadmap-refresh-sk` is new.
- **`/roadmap-refresh-sk`** — in a speckit repo a refresh finding has four possible homes, and
  choosing between them is the judgment the skill adds: the roadmap, `spec.md` (via clarify, never a
  hand-edit), `plan.md`/`research.md` (naming the decision id it overturns), or the constitution (an
  amendment with a version bump, never a dilution). It runs `/speckit-analyze` first as free
  staleness detection and scans for drift between the artifacts and the code — including the two
  mechanical signals worth running every time, a *ticked* task whose code does not exist and an
  *unticked* task whose code does.
- **A guard on the `-sk` pairing** — the `-sk` skills are delta documents, and a variant that stops
  naming the base it specializes has quietly become a second copy of it. `skills.rs` now asserts
  each one points a reader at its base's `SKILL.md` and says how it detects a Spec Kit project.

## [0.3.0] — 2026-07-25

### Added

- **`think-and-ship skills install` / `skills list`** — installs the bundled agent skills (`/roadmap`, `/roadmap-refresh`, `/signals`, `/business-intel`, `/handoff`, `/craft`, `/gui-blueprint`, `/gui-scrutiny`) into a coding agent's **user-level** skills directory: `~/.claude/skills` (Claude Code), `~/.codex/skills` (Codex), `~/.copilot/skills` (GitHub Copilot CLI). With no `--client`, every agent detected under the home directory is written; `--client all` writes all three. `--only <skill>` narrows to one skill, `--dry-run` previews, `--force` re-syncs a locally edited skill (bundled files are rewritten; files you added alongside them are kept). Home resolution is `HOME`, then `USERPROFILE`, then `HOMEDRIVE`+`HOMEPATH`, so the command works on Windows as well as Unix.
- **Skills bundled into the binary** — sources live in `crates/think-and-ship/skills/`, embedded at build time by `build.rs`, so `cargo install think-and-ship` carries them with no repo checkout. Agent differences are handled by client tokens (`{{SKILLS_DIR}}`, `{{TOOL_SEARCH}}`, `{{ASK_USER_TOOL}}`, `{{COAUTHOR}}`) substituted at install time, plus per-client overlay files under `crates/think-and-ship/skill-clients/<client>/` (Codex's per-skill `agents/openai.yaml` manifest).

### Changed — BREAKING

- **Cross-reference fields are named for the family they point at.**
  `ship_record` takes `think_step`, `ship_plan` takes `think_branch`,
  `ship_status` emits `think_refs`, and `roadmap_record_refresh` takes
  `think_steps` — each naming the `think_*` step or branch it links to. The
  record envelope contract (`contract/unified-record-envelope.schema.json`) and
  both generated validators carry the same names. No alias is accepted: a
  stored action whose cross-reference used an older spelling loses that link on
  load.

- **`tools/list` returns 53 tool definitions across four families.**

### Fixed

- **The server no longer misreports its own surface.** `SERVER_INSTRUCTIONS` —
  which several MCP clients paste into the model's system prompt — advertised
  `roadmap_* (8 tools)` against a real 12, because the roadmap family's
  instruction list had never gained `roadmap_start_chunk`,
  `roadmap_complete_chunk`, `roadmap_link`, or `roadmap_record_refresh`. Both
  are corrected, and a test now binds the advertised count per family to the
  real `list_tools_view()` count so neither side can drift alone.
- **Generated `CLAUDE.md` covers every family.** It claimed "three tool
  families" and omitted `signal_*` entirely; it now iterates the family list,
  with a test asserting every served family appears. The README pair likewise
  said "34 canonical tools across three families" — it is 44 across four.

## [0.2.0] — 2026-05-27

### Added

- **Unified server (`think-and-ship` crate, v0.2.0)** — single binary exposing every tool family through one `UnifiedService` that routes by name prefix.
- **`think-and-ship serve`** — runs the MCP server on stdio.
- **Family-tagged broadcast** — one Unix socket emits NDJSON frames with `{ "family": "think" | "ship", ... }` so a single viewer reads both halves.
- **Typed `CrossRef`** — internal enum (`ThinkStep` / `ShipTask` / `ShipAction` / `ShipCheck`) replaces the string-only `execution_ref` at use sites; the wire form (`task:foo`, `action:42`, `check:cargo-test`) is preserved.
- **`ToolFamily` trait + `FamilyRegistry`** — namespaced tool families register via composition (OCP) without modifying the wire adapter.
- **End-to-end rmcp client test** — pairs a real rmcp client with the server over `tokio::io::duplex` and verifies tools/list + dispatch through actual wire serialization.
- **`docs/ARCHITECTURE.md`** — design contract for the architecture.

### Changed

- **Persistence layout** partitioned: think writes to `<data_dir>/think/sessions/`, ship to `<data_dir>/ship/sessions/`. Both previously wrote to `<data_dir>/sessions/` and could clobber each other on shared `<project_id>.json` filenames — the dedicated subdirs eliminate that collision.
- **Internal module** `crate::engine` (shared infrastructure: project_id, persistence, broadcast, cross_ref) renamed to `crate::infra` to disambiguate from the per-family reasoning engine (`crate::think::engine`) and execution engine (`crate::ship::engine`).
- **`docs/ARCHITECTURE.md`** auto-session description now matches the actual `<basename>-<6hex>` stable-id behavior (the timestamped form was design intent, never shipped).

### Removed

- **Dual broadcast sockets** — a single `<data_dir>/broadcast.sock` with family-tagged frames replaces the per-family sockets.

### Fixed

- **Persistence path collision** between think and ship traces sharing a `<project_id>.json` filename under the same `data_dir`.

[0.2.0]: https://github.com/AlrikOlson/think-and-ship/releases/tag/v0.2.0
