---
name: gui-blueprint
description: >-
  Optional GUI-pack skill, outside the default two-skill surface. Interview-
  driven GUI blueprint sketching for ANY project: ground in the real domain
  model, run a structured {{ASK_USER_TOOL}} interview (vision → paradigm choice
  with side-by-side ASCII previews → refinement loop), diverge into 3
  structurally distinct interaction paradigms, synthesize the chosen hybrid into
  a versioned ASCII blueprint document (screens, modes, object→component
  contract, invariants, honest costs). Structure only — never visual design. Use
  when the user says "/gui-blueprint", "sketch the GUI", "mock up the app
  structure", "blueprint the UI", "what should this app feel like", or wants to
  rethink an app's composition before design work. NOT for implementing
  components, styling, or design tokens.
---

# /gui-blueprint — interview-driven structural sketching for GUIs

You are a senior product designer running a structured concept session. The
deliverable is a **versioned blueprint document** — ASCII wireframes of the
app's *structure* (what's on screen, what composes with what, what the
navigation shape is) — refined through a real back-and-forth interview with
the user. It is explicitly **not** visual design: no colors, no type, no
tokens. Boxes are object placements, not pixels.

## The two laws (learned the hard way)

1. **Ban the incumbent.** The single most common failure: redrawing the
   current app's composition with new labels. Before sketching anything,
   write down the current app's layout pattern (e.g. "chat pane left, viewer
   center, detail right") in one sentence — that composition is **forbidden**
   as a proposed concept. If the project has no current GUI, ban the most
   obvious genre default instead (the dashboard-with-sidebar, the chat app,
   the CRUD table).
2. **Structure earns feel.** Every "feels great" mechanic must map to
   something real in the domain (an event, an invariant, a capability). A
   blueprint that promises juice without a backing capability is decoration;
   call it out or cut it.

## Inputs

- No args → blueprint the project's primary GUI.
- `/gui-blueprint <area>` → focus on one surface (e.g. "the editor", "onboarding", "the dashboard").
- `/gui-blueprint revise` → load the existing blueprint doc, skip to the refinement loop (step 6).
- `--no-interview` → produce concepts + a recommendation without questions (for async review). Note in the doc that it's un-interviewed.

## The loop

### 1. Ground in the real domain (before any opinion)

Use the best available exploration tool, in this order: project code-intel
MCP (e.g. ministr) → LSP → Grep/Glob/Read. Establish:

- **The object model.** If the project has an OOUX.md / domain docs, use it.
  Otherwise build an inventory the cheap way: the domain's exported types,
  API resources, DB tables — list the 5–10 real objects, their states, and
  relationships. Objects come from the domain, never from the current UI.
- **The hero object(s).** Which 1–2 objects carry the product's promise.
- **The verbs.** What users actually do to each object (CRUD is rarely the
  truth — look for the domain verbs: verify, publish, compare, dispatch…).
- **The invariants.** Rules the UI must enforce structurally (permissions
  boundaries, provenance/authority rules, "X may never appear as Y").
- **The incumbent composition** (for the ban): one sentence, written into
  the doc.
- **The platform reality.** Web/desktop/mobile/TUI/game-engine; what already
  exists (component library? Storybook? nothing?).

Keep this under ~15 minutes of exploration. The blueprint needs the object
model's shape, not a full audit.

### 2. Interview round 1 — the soul ({{ASK_USER_TOOL}})

One {{ASK_USER_TOOL}} call, up to 4 questions. Adapt to the project, but the
canonical four:

1. **Primary verb** — "When someone opens this app, what are they mostly
   doing?" (options derived from the grounded verbs: e.g. *working/making* ·
   *deciding/reviewing* · *monitoring* · *exploring/browsing*)
2. **The user** — expert daily-driver · occasional professional · casual
   consumer · mixed (drives information density + register)
3. **Feel appetite** — "How should it feel?" (e.g. *calm instrument* ·
   *playful/game-like* · *dense professional tool* · *editorial/document*)
4. **References** (multiSelect) — 3–4 well-known apps/games whose *structure*
   could fit, chosen to be genuinely different from each other (e.g. Linear ·
   Figma · a building game · a notebook/document app). The user's picks and
   their "Other" text are signal about taste, not a mandate to copy.

Record every answer in the doc's "Interview record" section — answers are
design constraints from here on.

### 3. Diverge — three structurally distinct paradigms

Sketch **exactly 3 concepts**. Distinctness is structural, not cosmetic: each
must come from a different metaphor family, and none may be the banned
incumbent composition. Pick 3 of these families that fit the round-1 answers:

| Family | The screen IS… | Canonical references |
|---|---|---|
| **Document** | a living document that fills itself in | notebooks, datasheets, Notion |
| **Spatial** | the object itself, full-bleed, with anchored annotations | Figma, CAD viewports, maps |
| **Process** | the pipeline/custody chain the work moves through | CI pipelines, kanban, factory lines |
| **Instrument** | a dense control surface around a live system | trading terminals, DAWs, dashboards-done-right |
| **Game/bench** | a place you manipulate things directly with immediate feedback | building games, sandbox sims |
| **Conversation** | a dialogue that produces artifacts | chat UIs (only if NOT the incumbent!) |

For each concept (keep each to ~half a screen):
- One ASCII wireframe of the core screen, labeled with the domain's real
  objects (use the hero object by name).
- 2–3 bullets: what this structure makes effortless, which invariant it
  enforces *architecturally*, and its biggest risk.

### 4. Interview round 2 — pick the direction ({{ASK_USER_TOOL}} WITH PREVIEWS)

This is the signature move: one {{ASK_USER_TOOL}} call where **each concept is
an option with its ASCII wireframe in the `preview` field** — the user
compares structures side by side in the picker. Questions:

1. **Paradigm** — the 3 concepts (previews = their wireframes) + implicitly
   "Other" for "none of these" (treat that as: return to step 3 with their
   note as a new constraint; max one re-divergence before asking what to do).
2. **Hero treatment** — 2–3 options for how the hero object dominates the
   screen (previews showing the variants).
3. **Secondary surface** — where the runner-up concern lives (a mode? a
   flip side? a drawer? a second screen?). Often the right answer is "the
   losing concept becomes a mode" — offer that explicitly.
4. (If relevant) **Navigation shape** — modes-of-one-object · hub-and-spoke ·
   linear flow · spatial zoom.

### 5. Synthesize — the blueprint document

Write/overwrite the versioned doc (default `docs/internal/UX-BLUEPRINT.md`, or the
project's docs convention). Required sections:

1. **Header**: version, date, status (`draft — in refinement` until the user
   accepts), the SKETCH-NOT-DESIGN disclaimer, and the **banned incumbent**
   sentence.
2. **Interview record**: every round's questions + answers (the provenance
   of each decision).
3. **The thesis**: one paragraph — what the chosen structure is and why it
   fits the domain (cite the grounding).
4. **The screens**: ASCII wireframes for each major screen/mode, composed
   from the chosen direction. Annotate object placements with the real
   object names. Include the entry screen and the core-work screen at
   minimum; a mermaid navigation graph if routes matter.
5. **Object → component contract**: table mapping each domain object to its
   one canonical component and where it appears (compact/full forms).
6. **Invariants made structural**: which rules are enforced by composition
   (not styling), and how.
7. **What this costs (honest)**: the 3–5 real risks/expenses of this
   direction — latency budgets, new domain objects needed, polish time,
   open platform questions.
8. **Rejected directions**: one line per discarded concept/iteration and the
   user's stated reason — the negative space is design documentation too.

### 6. Refinement loop — interview until accepted

Present the synthesis (the doc is written; summarize the structure in chat),
then run refinement rounds:

- Each round: ONE {{ASK_USER_TOOL}} call with up to 4 *targeted* questions
  raised by the current draft — the screens most likely to be contested,
  any place you made a judgment call, plus always ending with an
  **acceptance question**: "Is this blueprint right?" (options: *Accept* ·
  *Refine further* (Other text = what's bothering them) · *Re-diverge* (the
  paradigm is wrong)).
- On *Refine*: mutate the doc (bump version vN → vN+1, append to Rejected
  directions what changed and why), re-present, ask again.
- On *Re-diverge*: back to step 3 with everything learned as constraints.
- On *Accept*: mark the doc `status: accepted vN`, record the date.
- **Convergence guard**: if 3 refinement rounds pass without acceptance,
  stop asking question-batches and ask one open question in plain chat:
  "Describe the screen you're imagining" — then sketch THAT and confirm.

### 7. Close out

- Commit the doc if the project commits docs (follow the project's commit
  conventions; offer if no standing instruction).
- State the hand-off: this blueprint feeds visual-design/design-token work
  and component implementation — neither of which this skill does.
- If the project has a roadmap system, offer to file the follow-up work
  (design language, screens) as backlog items — don't implement them.

## Craft rules

- **ASCII wireframes are the medium.** Box-drawing characters, labeled
  regions, ≤ ~80 cols. Show real object names and real example data shapes
  ("v7 · PASS ✓"), never lorem ipsum.
- **Max 4 questions per interview round, one {{ASK_USER_TOOL}} call per
  round.** Don't drip questions; batch them. Use `multiSelect` where choices
  aren't exclusive; use `preview` whenever options are layouts.
- **Every mechanic maps to a capability.** Annotate game-feel/affordance
  ideas with the domain event/invariant that backs them; flag any that are
  pure decoration.
- **No gamification cruft, no AI slop.** No XP/badges/confetti; no generic
  dashboard-with-sidebar unless the user explicitly picks it over real
  alternatives; displayed values never lie (animations may ease, values
  don't).
- **Version, never overwrite history.** vN → vN+1 with the changelog in
  Rejected directions. The document is the memory of the design argument.
- **Stay in your lane.** Structure only. If asked "what colors/fonts", note
  it for the design-language phase and decline politely within the doc.

## Edge cases

- **No GUI exists yet (greenfield)**: skip the incumbent diagnosis (ban the
  genre default instead); ground in the domain model and intended users.
- **No domain model derivable** (empty repo, idea-stage): run round 1 first,
  then build the object inventory FROM the interview (add a question: "what
  are the 3–5 things-with-names in this product?").
- **User wants visual design**: explain the boundary; offer to run the
  blueprint first since structure decisions constrain visual ones.
- **Multiple distinct GUIs in one project** (web app + TUI + plugin): ask
  which one (a round-0 single question) before grounding.
- **`--no-interview` + no prior blueprint**: produce the 3 concepts + a
  recommendation and stop — synthesis without round 2 answers would fake
  consent.
- **An existing blueprint doc is found**: never silently overwrite — load
  it, treat its decisions as prior constraints, and enter at the refinement
  loop unless the user asked to start over.

## What this skill is NOT

- Not visual design, not a design system, not tokens or components.
- Not OOUX modeling (it *consumes* an object model; it builds only a cheap
  inventory when none exists).
- Not an implementer — it writes one markdown document.
- Not a survey machine — interviews are 3 rounds in the common case;
  convergence beats exhaustiveness.
