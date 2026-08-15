---
name: craft
description: >-
  Now internal doctrine that the core skills apply. Kept for existing users. The
  operating doctrine (house style) for any project built on the think-and- ship
  + ministr + serpapi MCP triad. Encodes how to work — the tri-MCP interleave
  (think to frame, ministr to ground, serpapi for 2026 research, ship to track),
  atomicity, commit-on-main discipline, verify-with-the-real-gate, honest
  accounting, and — for projects with a GUI — an object-first OOUX model (the
  ORCA lens: objects → relationships → CTAs → attributes) that organizes
  Storybook by object, Storybook-everything built from design tokens, plus
  empirical Playwright scrutiny (visual AND mechanical, incl. object-model
  fidelity) as an expert UX critic. Invoke `/craft` at the start of a working
  session to load the doctrine, or when the user says "apply the usual way of
  working", "the house rules", "do it the way we always do". The /roadmap and
  /roadmap-refresh skills apply this doctrine automatically; /gui-scrutiny is
  its GUI verify procedure.
---

# /craft — the operating doctrine

This is the **house style** — the standing way of working the user expects on every
project that uses the **think-and-ship**, **ministr**, and **serpapi** MCP servers.
It exists so the user never has to re-type the same directive each session.

Two tiers:

- **§A — ALWAYS** rules apply to *every* project on the triad (Rust, C++, TS, anything).
- **§B — GUI** rules apply *only when the project has a user interface* (a Storybook,
  a web app, a Tauri/Electron desktop app, a game HUD). Self-gate: if there is no UI
  layer, skip §B entirely — do not invent Storybook/Playwright work for a headless
  library or an MCP server.

Detect the GUI tier by looking for a `.storybook/` dir, a `storybook` script in
`package.json`, a Tauri `src-tauri/`, or an obvious front-end app dir. When unsure,
ask once.

This doctrine is *orthogonal* to the loop you happen to be running. It composes with
`/roadmap` (implement a chunk), `/roadmap-refresh` (reshape the plan), and plain
ad-hoc work alike.

---

## §A — Always (every project on the triad)

### A0. Staged tool loading — never bulk-load a family up front

**Load the tools a phase calls, at the phase that calls them. Never a whole
family "so it's ready".**

Why this is doctrine and not taste. Modern hosts defer MCP tool definitions and
load them on demand — Anthropic's tool-search tool, shipped in Claude Code as
`ENABLE_TOOL_SEARCH`, cut one measured session from 51k to 8.5k tokens (46.9%;
anthropics/claude-code#12836) and Anthropic reports up to 85%. think-and-ship's
own surface is 48 tools / ~88 KB / ~21k tokens, and a client with deferral pays
none of it up front.

A skill that opens with one big `select:` **throws that saving away** — it
reloads most of the payload before the run knows what it needs. Our own skills
did exactly this: 132 tool selections across five bundled skills, every one of
them paid at step zero. The client solved progressive discovery and our
instructions un-solved it.

The rule, in three parts:

1. **The opening select covers phase one only** — typically the status/read
   tools plus `think_record_step`. Three to five tools, not thirty.
2. **Mutation verbs load after the decision, never before it.** You cannot know
   which of `roadmap_add_chunk` / `_update_chunk` / `_obsolete_chunk` /
   `_reprioritize` a run needs until synthesis says so. A dry run loads none.
3. **"Just in case" is the anti-pattern.** If you cannot name the call site,
   don't load the tool.

Cost model, so this is a judgment and not a ritual: a `select:` costs one
round-trip and loads the named schemas. Several small selects beat one large
one whenever the large one loads tools the run never calls — which is the normal
case for any skill with branches. When a phase genuinely calls eight tools, load
eight in one call; that is staging, not a violation.

This applies to **every** skill on the triad, including ones written later. A
new skill's step 0 is expected to look like a short opening select plus a table
of load-at-point-of-use lines.

### A1. Interleave the three MCPs — don't bundle research into a preamble

At **every non-trivial decision point**, cycle the three servers; don't front-load one
big research phase and then code blind:

1. **think** (`think_record_step`) — frame the question. Capture purpose, context,
   current thought, planned next action, rationale, and a `confidence` when uncertain.
   Open and close steps are mandatory for any substantial task; mid-flight steps for
   load-bearing decisions. Step numbers are **project-global** — check
   `think_engine_status` first and use the next integer. Pin (`think_pin_step`)
   genuinely load-bearing findings; `think_checkpoint` long traces.
2. **ministr** — **ground it in the real code** before believing anything.
   `ministr_survey` for "where/how does X happen?", `ministr_symbols` →
   `ministr_definition`/`ministr_read` for specifics, `ministr_references` **before**
   touching shared code, `ministr_bridge` **before** any cross-language (IPC/FFI)
   boundary. ministr is the **required** exploration surface — not Grep/Glob/find.
3. **serpapi** — validate any externally-facing choice against **2026 norms** (library
   APIs, standards, best practice, prior art). Default to current-year results. Skip
   only when the question is purely internal mechanics.
4. **think** again — synthesise (a `decision`/`summary` step citing the prior steps as
   dependencies).

> **Ground before you research the alternative.** Confirm what the code *actually*
> does today (ministr) before researching a replacement (serpapi). The single most
> common waste is "redo everything" energy spent on something that was already fine —
> grounding first prevents it.

Track *execution* in parallel with **ship** (`ship_set_objective` → `ship_plan` →
`ship_start`/`ship_record`/`ship_check` → `ship_finalize`). Cross-reference the
families: `think` steps carry `execution_ref: "task:<id>"`; `ship_record` carries
`think_step: <n>`. The combined trace is one graph.

### A2. Atomicity — one shippable thing per pass

Do **one** coherent, independently-verifiable chunk end-to-end, then stop. When a
chunk sprawls past that, **split it** (file the sub-chunks with deps, narrow the
current scope) rather than ballooning the change. Stopping at a clean, committed,
green boundary always beats a half-finished giant.

### A3. Commit often, on `main`

The standing instruction on these projects is **commit frequently, always on `main`**
(unless the user says otherwise). Commit each atomic chunk once its gate is green.
End every commit message with the project's trailer — for the user's projects:

```
Co-Authored-By: {{COAUTHOR}}
```

If your harness names a concrete model identity for the trailer, use that
instead — it is the more precise attribution.

Commit code in the repo that **owns** the file; if a change spans repos, make
**separate** commits in each. Never push without being asked.

### A4. Verify with the project's REAL gate — and never mask it

- Use the project's **canonical** gate, not a convenient proxy: e.g. `just validate`
  / `cargo test && cargo clippy -- -D warnings` (Rust), `pnpm test && tsc --noEmit &&
  build` (the real package manager, not a stray `npm`/`npx`). If you can't run the
  canonical gate, mark the check **UNVERIFIED** — don't claim green.
- **Never mask an exit code.** Don't pipe a gate to `tail`/`head` or append `echo`
  that swallows the status — that manufactures false green. Run it bare or redirect to
  a file and read the file. A failing gate means the chunk is **not** done.
- Record gates with `ship_check`; a required failed check blocks `roadmap_complete_chunk`.

### A5. Honest accounting — surface the negative findings

- Report outcomes faithfully: if tests fail, say so with the output; if a step was
  skipped, say that; state "done + verified" only when it is.
- Don't punt discovered breakage as "pre-existing / out of scope" — if you touched the
  area and it's broken, **fix it in-tick** (and run the formatter before committing).
- An audit that honestly reports "mostly fine, here are the 2 real gaps" is more
  valuable than a fabricated overhaul. Record the honest negatives in the closing
  `think` step.

### A6. Huge transformations, object-first (OOUX) framing

The user wants **ambitious, structural** moves, not timid patches — but each delivered
**atomically** (§A2). Frame the work in **objects**, not screens or CRUD. This is the
**OOUX** lens (Sophia Prater's ORCA), and it applies to *every* project on the triad —
the GUI projection (Storybook-by-object) is §B0, but the noun-first discipline is universal:

- Identify the domain's real first-class **objects** (the nouns users care about) and make
  each a coherent thing with its own affordances. Ground the names in the real code/schema
  with `ministr` — reuse the domain's existing nouns; don't invent parallel vocabulary.
- See each object through the four **ORCA** facets, because they map cleanly onto code:
  **objects** → modules/types, **relationships** → typed links (cardinality drives
  composition), **CTAs** → the actions/commands/routes on the object, **attributes** →
  its fields. A change is "structural" when it sharpens this model, not just the pixels.
- **One renderer per object.** When two code paths render the same object, unify them
  on a single component with a mode/zoom prop. A render unreachable in the new
  structure is **dead** — delete it, don't preserve it "just in case".
- Prefer composing existing capability over adding a near-duplicate surface
  (a new tool/route/command that mirrors an existing one is usually the wrong cut).

> Why object-first: intuitive UX requires **recognizable, connected, valuable objects**
> (OOUX manifesto). Modeling objects up front exposes complexity early — cheap — instead
> of mid-build, where pivots are expensive (§A8's spec discipline, applied to IA).

### A11. Single source — derive, don't duplicate

A value two or more components must agree on (a cross-part station, a shared threshold, a
layout constant, a design token) lives in **one** place — a datum / skeleton / tokens file
— and everything else **derives** it through an accessor. Never copy the number into a
sibling file; copies drift silently and one edit becomes an N-file scavenger hunt. When the
platform's import mechanism can't see the shared declaration (e.g. OpenSCAD `use<>` drops
bare top-level constants; a build that inlines env at compile time), expose an accessor it
CAN see. Make duplication a **failing gate**, not a code-review hope. Generated artifacts
(mate sidecars, manifests, lockfiles) are build outputs of the source — regenerate them,
don't hand-edit. (Concrete: the Kitbash car is one `car_datum.scad`; `just check` fails on
a hardcoded station — see `/fab-part` "Datum-as-skeleton".)

### A7. Memory

Persist durable, non-obvious facts (user preferences, hard-won lessons, project
constraints, standing rules) to the agent's file memory so they survive across
sessions. Don't re-record what the repo/git already says.

### A8. The acceptance criteria are an executable spec — verify against them

The 2026 norm is **spec-driven development**: `spec → design → task-plan →
implement → verify`, with the spec as an *executable contract* that constrains what
gets built (not passive docs). The roadmap chunk model already encodes this — a
chunk's **`acceptance[]` is the spec**.

- **Confirm/sharpen the acceptance before writing code.** If it's vague, tighten it
  (`roadmap_update_chunk`) first — a fuzzy spec yields fuzzy work.
- **Verify the finished chunk against each criterion**, explicitly (`ship_check`), and
  state which criteria are met. "Done" means the contract is satisfied, not "it runs".
- **Don't silently drift.** If implementation reveals the spec is wrong, change the
  spec *deliberately* (a `think` step + `roadmap_update_chunk`), don't quietly diverge.
- Keep the spec/plan/decision trail in think+roadmap so context survives across
  sessions and across agents.

### A9. Delegate isolated, context-heavy work to subagents — protect the main thread

Skills inject procedural knowledge into **this** (main) context; **subagents** run in
**isolated** context windows. Reach for a subagent (the Agent tool — `Explore` /
`general-purpose`) when the task is:

- **broad exploration** that would flood the main context ("read 30 files to find
  every call site"),
- an **independent review / adversarial check** — a fresh-eyes critic that didn't
  write the code (the editor/checker pattern), or
- **parallel fan-out** of independent sub-tasks.

Keep the *synthesis and the decisions* in the main thread; subagents return
conclusions, not raw dumps. **Don't over-delegate** — a human-in-the-loop checkpoint
beats blind fan-out (the 2026 caution against agent sprawl). On a **ministr** project,
`ministr_*` *is* the context-efficient exploration surface — prefer it over a
file-reading subagent for code questions.

### A10. Human-in-the-loop checkpoints

Ambitious moves (§A6) still pause for the human at the decision points: a refresh
*proposes* and stops; re-prioritization is a *proposal*, never auto-applied; an
outward-facing or hard-to-reverse action is confirmed first. Advance one atomic chunk,
then surface state — don't run the whole plan unattended unless asked.

---

## §B — GUI projects only (Storybook / web / Tauri / desktop / game UI)

Gate: only if the project has a UI layer (see detection above). For headless
libraries, CLIs, and MCP servers, **skip §B**.

### B0. Object-first — an ORCA-lite pass before pixels (OOUX)

Before composing screens, model the **objects**. OOUX (Sophia Prater's ORCA) is the
*top-down* complement to B1's *bottom-up* tokens-and-atoms: identify the domain's real
**nouns** and design them as first-class things *before* laying out views. The UI is then
an arrangement of **recognizable, connected, valuable objects** — not a pile of screens.

For any new surface, run a **lightweight ORCA pass** (minutes, in a `think` step — not a
workshop or a deliverable doc), in order:

1. **Objects** — the system's core nouns (the things users point at). Ground them in the
   real domain with `ministr`; reuse the names the code/schema already uses.
2. **Relationships** — how objects connect and nest, with cardinality (a Catalog *contains
   many* Blocks; a Part *has many* Mates). This drives navigation and composition.
3. **CTAs** — what the user can *do* to each object (the verbs) → its actions/affordances,
   mapped to the project's real commands/routes.
4. **Attributes** — the object's content + metadata fields → its props and the variant axes
   of its component.

The output is a one-screen **object map** (objects × attributes × CTAs × nested objects).
It exposes complexity early (cheap) instead of mid-build (expensive), and it **is the
blueprint** for the Storybook structure (B2), the "one renderer per object" rule (§A6), and
the object-model fidelity bar gui-scrutiny verifies (`/gui-scrutiny` §2.5). Full procedure +
the **Object↔Storybook mapping** + the fidelity checklist: **`reference/ooux.md`**.

### B1. Build from design tokens & atoms — never reuse ad-hoc compositions

New UI is composed from the design system's **tokens** and **atoms** (primitives),
not copy-pasted from an existing bespoke composition. Honor the project's documented
design floor (e.g. a `DESIGN.md`, a token contract, a design-lint gate). The result
should look like it was designed, not assembled.

### B2. Storybook-everything — organized by object

**Every component gets a Storybook story** — light **and** dark, plus the states that
apply (idle · hover · active · empty · loading · error · disabled). A **story-less
component is a scrutiny blind spot**: either add the story or, if the component is
unreachable, delete it. Composed/screen-level stories should render **rich** (real
data shapes via the project's mock harness), not empty shells.

**Structure the hierarchy by object (B0), so Storybook *is* the object model made
visible:** each core object is a top-level section holding its **one canonical renderer**
(§A6) at every **zoom** (chip · row · card · detail) — the appearances must read as the
*same recognizable object* — then its **CTAs** as interaction stories and its
**relationships** as composition stories (a parent object's story renders its children via
the *same* child components, never a bespoke re-render). Atoms, tokens, and primitives live
under a **Foundations** section beneath the objects. (Mapping detail: `reference/ooux.md`.)

### B3. Scrutinise empirically — two complementary modes

Before calling any GUI work finished, verify it **empirically**, in **light and dark**,
in the two ways the 2026 stack distinguishes:

1. **Automated regression gate** — the **Storybook Vitest addon** turns your stories
   into real Vitest tests run in **browser mode via Playwright's Chromium**, and
   **`@storybook/addon-a11y` runs axe on every story** in that same run. This is the
   mechanical net that lives in the project's gate / CI — wire it in and keep it green.
   (Storybook docs, 2026: "automating them is as simple as running them in CI.")
2. **Exploratory UX scrutiny** — drive the running Storybook with the **Playwright
   MCP** for what the addon can't express: screenshot-and-critique like a designer
   (B4), and **bespoke mechanical probes** — `document.activeElement` after a keypress,
   command-invocation counts, persisted classes/state, forced-colors survival,
   `0` console errors. A claim like "no dead ends" or "keyboard nav works" must be
   backed by a DOM assertion, not a glance.

Both are the **bar for done**: the addon gate catches regressions cheaply at scale; the
MCP scrutiny catches what only a human-style read + a bespoke probe can. The reusable
procedure is **`/gui-scrutiny`** — invoke it as the verify stage of a GUI chunk, or
standalone.

### B4. Critique like a UX expert — not "does it render"

When reviewing in Storybook, evaluate as a senior product designer would:
visual **hierarchy**, **alignment** & spacing rhythm, **consistency** across views,
**contrast**/legibility, **affordances** (does it look interactive where it is?),
**empty/loading/error** states, and **microcopy**. Report what's weak and fix the
bounded set — confirming it renders is necessary but not sufficient.

Inspect **at two scales**: the full frame for *composition*, and a **component-scoped
crop** for *detail*. Small type — labels, numbers, badges/`kbd`, captions, icons — is
sub-legible on a full-frame screenshot, so judging its typography from one is
rubber-stamping; crop to the element (and/or measure with `getComputedStyle`) and read
every glyph (size/weight/case/family/spacing) against the design-system grammar. **If
you can't read it in the screenshot, you haven't reviewed it.** (Procedure: `/gui-scrutiny` §2.3.)

### B5. Cohesion across views — same object, same grammar everywhere

Features and views should feel like **one** product: a shared chrome grammar
(header/empty/loading/refresh), consistent navigation conventions (e.g. row-click vs
an explicit secondary action), and one spacing/typography rhythm. When a pattern
proves out in one view, extract it and apply it across the others. Cohesion is
**object-level first**: a given object (B0) must be **recognizable as the same thing**
wherever it appears — same renderer (§A6), same CTA verbs, same attribute vocabulary —
so the user builds one mental model, not one per screen. An object that looks/behaves
differently across two views is an incoherence finding, not a style nit.

### B6. 2026-only research for the front-end too

Front-end choices (framework features, a11y norms like WCAG 2.2 / forced-colors,
perf techniques like auto-memoization / virtualization, component-library SOTA) are
validated against **2026** sources via serpapi — and grounded against the **installed**
version in the repo (don't assume "latest"; the installed major can change the right
answer, e.g. a build-plugin that swapped its transformer between majors).

---

## How this composes

- **`/roadmap`** — advance one roadmap chunk. Applies §A throughout; for a GUI chunk,
  the verify stage is **`/gui-scrutiny`** (§B3).
- **`/roadmap-refresh`** — research & reshape the plan. Applies §A1 (interleave) and
  §A5 (honest negatives) hardest; files §B gaps as backlog chunks rather than fixing
  them inline.
- **`/gui-scrutiny`** — the standalone empirical UX-audit procedure for §B3–B5.
- **Ad-hoc work** — even outside the roadmap loop, §A (interleave, atomicity,
  commit-on-main, real-gate verify, honesty) and §B (when there's a UI) still hold.

## What this is NOT

- Not a task runner — it changes *how* you work, not *what* gets done. The "what"
  comes from `/roadmap`, the user's request, or the roadmap state.
- Not a license to skip verification or sprawl scope — §A2 and §A4 are hard floors.
- Not GUI-mandatory — §B self-gates off for headless projects.

---

*Grounded in June-2026 norms (serpapi): Anthropic skill-authoring best practices
(progressive disclosure, lean SKILL.md); spec-driven development as the dominant
agentic-coding paradigm (Turing Post / Augment / thebcms, May 2026); the
Storybook Vitest-addon + addon-a11y testing stack (storybook.js.org docs); the
skills-vs-subagents context model (gitconnected / olioapps / towardsai, 2026); and
Object-Oriented UX / the ORCA methodology (Sophia Prater, ooux.com) as the object-first
IA discipline behind §B0. Last refreshed 2026-06-08.*
