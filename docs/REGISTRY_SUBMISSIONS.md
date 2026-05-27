# Registry submission drafts

Copy-paste-ready submission text for the four MCP registries Phase 18
targets. Submit **after** `cargo publish` and `npm publish` of
think-and-ship v0.2.0 — most registries link to the registry listings,
not the source.

The drafts below assume:

- Repo: `https://github.com/AlrikOlson/think-and-ship`
- npm: `https://www.npmjs.com/package/think-and-ship`
- crates.io: `https://crates.io/crates/think-and-ship`
- License: MIT
- Tags: `mcp`, `ai`, `agent`, `reasoning`, `execution`

Update the placeholders (✏️) before submitting.

---

## 1. GitHub MCP Registry (`modelcontextprotocol/registry`)

The official MCP registry maintained by the Model Context Protocol
project. Submissions are PRs to a YAML/JSON manifest.

**Workflow**

1. Fork the registry repo (URL: ✏️ check current upstream — was
   `modelcontextprotocol/servers` historically, may have moved to a
   dedicated `registry` repo by 2026).
2. Add an entry under the appropriate category (likely `community`
   servers).
3. Open a PR with the manifest entry below.

**Entry (YAML)**

```yaml
- name: think-and-ship
  description: >
    Unified MCP server that records structured reasoning (think_*, 11
    tools) and structured execution (ship_*, 11 tools) for AI agents.
    Pairs reasoning traces with task/quality-gate tracking and
    auto-correlates them by project identity.
  repository: https://github.com/AlrikOlson/think-and-ship
  install:
    npm: think-and-ship
    cargo: think-and-ship
  tags:
    - reasoning
    - execution
    - traces
    - audit
    - rust
  language: rust
  transport:
    - stdio
  tools_count: 22
  homepage: https://github.com/AlrikOlson/think-and-ship
```

---

## 2. Smithery (`smithery.ai`)

One-click MCP install for Cursor / Claude Code users.

**Workflow**

1. Sign in to https://smithery.ai with GitHub.
2. Click "Submit a Server" (or visit the submission form).
3. Smithery scrapes the npm package and repo automatically — you mainly
   confirm metadata.

**Manual override fields (if Smithery's scraper misses something):**

- **Name:** think-and-ship
- **Tagline:** One MCP server. Two tool families: reasoning + execution.
- **Long description:**
  > think-and-ship is a Rust-based MCP server that gives AI agents two
  > structured trace surfaces in one binary: `think_*` (11 tools) for
  > reasoning steps, branches, revisions, confidence, and dependencies;
  > `ship_*` (11 tools) for execution objectives, task plans, actions,
  > quality gates, and artifacts. The two halves cross-reference each
  > other automatically by project identity, so a single conversation
  > produces an end-to-end audit trail from "what the agent thought" to
  > "what shipped." Replaces the v0.1.x `deliberate-mcp` and
  > `resolute-mcp` packages with one unified server.
- **Repo:** https://github.com/AlrikOlson/think-and-ship
- **Install:** `npm install -g think-and-ship`
- **MCP config snippet:**
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
- **Tags:** reasoning, execution, traces, audit, rust, mcp

---

## 3. awesome-mcp-servers (`punkpeye/awesome-mcp-servers`)

Community-curated list. PR a one-line entry under the right section.

**Workflow**

1. Fork `punkpeye/awesome-mcp-servers`.
2. Find the section that fits — likely `## 🤖 AI / Agents` or
   `## 🛠️ Development Tools`. Look for similar-shaped entries to match
   the local convention.
3. Add the line in alphabetical order:

**Entry**

```markdown
- [AlrikOlson/think-and-ship](https://github.com/AlrikOlson/think-and-ship) - 🦀 - 🏠 - 🍎🐧 - One MCP server, two tool families: `think_*` records reasoning traces (steps, branches, revisions, confidence) and `ship_*` records execution traces (objectives, tasks, actions, quality gates). Auto-correlates by project identity.
```

Adjust the emoji legend (`🦀 Rust / 🏠 Self-hosted / 🍎🐧 macOS+Linux`)
to whatever awesome-mcp-servers uses currently.

---

## 4. mcpservers.org

Aggregator site for MCP servers. Submission process: ✏️ check current
form at https://mcpservers.org (may have a submission link in the
footer or a public form).

**Submission fields:**

- **Server name:** think-and-ship
- **Category:** Development tools / AI agents
- **Description (short):** Unified MCP server for AI agent reasoning + execution traces (22 tools across `think_*` and `ship_*` families).
- **Description (long):** Same as the Smithery long description above.
- **Repository:** https://github.com/AlrikOlson/think-and-ship
- **License:** MIT
- **Language:** Rust
- **Platforms:** macOS (arm64, x64), Linux (x64, arm64)
- **Install:** `npm install -g think-and-ship` or `cargo install think-and-ship`
- **Transport:** stdio (Streamable HTTP support tracked in Phase 19)
- **MCP spec version:** 2025-06-18 (compliance review for 2026-07-28 RC tracked in Phase 20)

---

## README badge placeholders

Once each registry listing is live, swap these placeholders into
[`README.md`](../README.md) near the existing CI / npm / crates.io
badges. Until then, leave the placeholders out so the README doesn't
link to 404s.

```markdown
[![Smithery](https://smithery.ai/badge/think-and-ship)](https://smithery.ai/server/think-and-ship)
[![mcpservers.org](https://img.shields.io/badge/mcpservers.org-listed-blue)](https://mcpservers.org/server/think-and-ship)
[![GitHub MCP Registry](https://img.shields.io/badge/MCP%20Registry-listed-blue)](https://github.com/modelcontextprotocol/registry)
```

URLs are best-effort — check each registry's actual badge format and
the canonical URL for the listing when submitting.

---

## Acceptance: when this phase is done

A developer searching "reasoning traces MCP" or "execution tracking
MCP" can find think-and-ship in at least **two** of these four
registries within a week of v0.2.0 publish.
