---
name: gui-scrutiny
description: >-
  Optional GUI-pack skill, outside the default two-skill surface. Empirically
  verify a GUI before front-end work is called done, and hunt AI slop while
  doing it: generic templated layouts, unmotivated gradients and shadows, emoji-
  as-UI, placeholder filler, fake metrics, off-scale spacing, default-component-
  library look with no brand grammar, marketing microcopy in a utility tool,
  accessibility theater. Two mechanisms: an automated regression gate (Storybook
  stories as browser-mode tests, axe on every story) and exploratory scrutiny
  driving the running Storybook in light and dark, proving behaviour with DOM
  assertions and checking object-model fidelity. Use when the user says
  "scrutinize the UI", "review it in Storybook", "check it with Playwright", "is
  this AI slop?", or as the verify stage of a GUI roadmap chunk. Probe recipes
  live in reference/probes.md.
---

# /gui-scrutiny — empirical Storybook + Playwright UX verification, religiously anti-slop

The **verify bar for GUI work** (the procedural arm of `/craft` §B3). A component or
view isn't done until it has been checked **empirically**, in **light and dark**, and
found **free of AI slop**. Three jobs, intertwined — the anti-slop directive is the lens
over the other two, not a separate pass:

| Job | Tool | Catches | Where it lives |
|---|---|---|---|
| **0. Anti-slop (prime directive)** | your taste + ministr grounding + slop probes | generic, templated, characterless, dishonest UI | **every** screenshot, every probe, every review |
| **1. Automated gate** | Storybook **Vitest addon** + `addon-a11y` (axe) | regressions + WCAG, cheaply, at scale | the project gate / CI |
| **2. Exploratory scrutiny** | **Playwright MCP** driving Storybook | design weaknesses + bespoke behavior a test can't express | this session, before "done" |

Use it standalone ("scrutinize the Explore views") or as the verify stage of a
`/roadmap` GUI chunk. If the project has no Storybook, this skill doesn't apply — say so.

---

## 0. The anti-slop doctrine (the prime directive)

**AI slop is the default output of a model asked to "make a UI." Your job is to refuse
it — religiously, across every facet.** Slop is not "ugly"; it's *characterless,
templated, and dishonest* — UI that looks generated rather than designed. It is the
single most common failure mode of AI-built front-ends, and it is **disqualifying**: a
view that renders, passes axe, and is riddled with slop is **not done**.

The bar: **could a senior product designer with taste have shipped this, on purpose,
inside *this* product?** If it looks like it came from a starter template, a model's
"dashboard" reflex, or a component-library demo — it fails.

### The slop taxonomy — tells you hunt for (any one is a finding)

**Layout & composition**
- Everything centered in a single `max-width` column with no information density; a
  utility tool laid out like a marketing splash.
- The reflexive **three feature-cards row** (icon + title + one-line blurb), symmetric
  for symmetry's sake.
- Hero section / oversized gradient banner on an internal tool.
- No deliberate hierarchy — every block the same weight, size, and emphasis.

**Surface & style**
- **Unmotivated gradients** (esp. purple→violet→indigo, the model's house style),
  glassmorphism, or neon glow with no reason in the brand.
- **Drop shadows on everything**; over-rounded corners everywhere; borders + shadow +
  fill all at once "to be safe."
- One-off colors and **off-scale spacing** (13px, 7px, 22px) instead of the token scale;
  hardcoded hex outside the token system.
- Default component-library look (unstyled MUI/Chakra/shadcn demo grammar) with **zero
  project grammar** — it could be any app.

**Content & data**
- **Lorem ipsum**, "placeholder", "Example", "Lorem", or obviously fake names/numbers.
- **Fake metrics** — stat cards with ▲ 12% trends that mean nothing; vanity dashboards.
- Data shapes that don't match the real domain (round numbers, "John Doe", `foo/bar`).

**Iconography & decoration**
- **Emoji used as UI** — as icons, bullets, status, or section markers. Near-always slop.
- Decorative icons with semantic roles (or vice-versa); icons whose meaning doesn't
  match the label; mixed icon stroke weights / families.

**Microcopy & tone**
- Over-friendly marketing voice in a utility ("✨ Welcome to your beautiful
  dashboard!"); exclamation marks; emoji in copy.
- **Redundant/obvious labels** ("Click here to…", "This button submits the form").
- Vague hedging instead of the real noun; Title Case Everywhere; inconsistent
  terminology for the same concept across the app.

**Honesty & a11y theater**
- `aria-label`s that lie or restate the visible text; `<div onClick>` pretending to be a
  button; alt text like "image".
- Empty/loading/error states that are absent, or over-explained with a paragraph and an
  emoji instead of one honest line.
- Animations/transitions that add nothing but latency.

### How to apply it
- **Ground before you judge.** Use `ministr` to find the project's *existing* grammar —
  tokens, spacing scale, type ramp, the real icon set, established components, the
  domain's real nouns and data shapes. Slop is, precisely, **divergence from that
  grammar toward the generic**. A "new pattern" that isn't already in the system is
  guilty until justified.
- **Name the tell, cite the evidence.** Every slop finding gets the specific tell
  (above), the screenshot/probe that proves it, and the in-grammar fix — not "feels
  generic."
- **It's a hard gate.** Slop findings block "done" exactly like a failing test. Fix the
  bounded set (§3); file the rest as backlog. "Renders + axe-clean + sloppy" ships
  nothing.

The rest of this skill is how you *find* it: the gate catches the mechanical tells at
scale (§1), the designer's read + probes catch the rest (§2).

---

## 1. The automated regression gate (Storybook Vitest addon)

The 2026-canonical UI gate: stories become **real Vitest tests**, run in **browser mode
via Playwright's Chromium**, and `@storybook/addon-a11y` runs **axe on every story** in
the same pass (Storybook docs, *Accessibility tests* / *Vitest addon*).

- If the project already wires this in (a `storybook` Vitest project + addon-a11y in the
  test run), **run it as part of the gate** and keep it green. A story added for a new
  component is automatically covered.
- If it's **not** wired in, wiring it is itself a worthwhile chunk — it turns the
  documented a11y floor into a mechanical one across every story at once. Prove it
  **fails on a seeded violation**, then passes clean, so the gate is real.
- **Extend the gate with mechanical anti-slop checks** — the slop tells that *can* be
  caught by a machine should be, so they never regress:
  - **token purity** — no hardcoded hex/rgb and no off-scale px in components (grep
    gate); spacing/color resolve to tokens only.
  - **no emoji in UI strings** — a unit test asserting component/story text and labels
    contain no emoji codepoints.
  - **no lorem/placeholder** — assert rendered copy doesn't match `/lorem|ipsum|placeholder|john doe|example\.com/i`.
  - **a11y-honesty spot checks** — interactive elements are real controls (role/button),
    not click-`div`s; `aria-label`s don't merely echo visible text.
- This is the **scale** layer: low-maintenance coverage of thousands of states. It does
  **not** replace §2 — axe + render-without-throwing + no-emoji is necessary, **not
  sufficient**. Taste is not lintable; that's §2.

## 2. Exploratory UX scrutiny (Playwright MCP)

For what the gate can't express: a designer's read (incl. the slop tells no linter sees),
and bespoke behavioral probes.

### 2.1 Boot Storybook (once)

Start it in the background on its port (usually 6006), then **poll** the iframe endpoint
until it answers before driving it. Story iframe URL (one story, isolated,
theme-controlled):

```
http://localhost:6006/iframe.html?id=<story-id>&globals=theme:dark&viewMode=story
```

`<story-id>` = kebab `title` + `--` + lowercased export (e.g. `Code/CodeViewer` →
`FocusLine` → `code-codeviewer--focus-line`). For view-level audits prefer the
**live/composed** stories (real data shapes via the project's mock decorator, e.g.
`withTauriMock` / an MSW-backed story) — composed stories with **real data shapes** are
also your best defense against the "fake data" slop tell. Resize to a representative size
(`browser_resize`, e.g. 1280×860).

### 2.2 Per target: load light AND dark

Load every target at `theme:dark` and `theme:light`. For async renderers (Shiki, data
fetches) **poll** in `browser_evaluate` for the real ready signal (a selector appearing)
— don't fixed-sleep.

### 2.3 Visual review — critique like a UX expert, hunt slop like a zealot

Screenshot (`browser_take_screenshot`) and **actually read** it against the `/craft` §B4
checklist **fused with the §0 slop taxonomy**: **hierarchy · alignment & rhythm ·
consistency · contrast · affordances · empty/loading/error states · microcopy — AND is
any of it generic, templated, gradient-sloppy, emoji-littered, fake-data, or
off-grammar?** Report what's weak with specifics and name the slop tell.

**Inspect at TWO scales — the frame AND the component. Mandatory.** A full-frame
screenshot proves *layout* (composition, hierarchy, balance, whitespace — and the
layout-level slop tells: centered-splash, three-card reflex, hero banner) but it
**cannot** prove *detail*: at 1:1 frame scale, 10–14px text is a few pixels tall, so
typography, glyph rendering, casing, padding, alignment, icon detail — and the
detail-level slop tells (emoji-as-icon, mismatched icon weight, Title Case, off-baseline
glyphs) — are **sub-legible; you will rubber-stamp them**. So for every element carrying
small type or fine detail — labels, numeric readouts, badges/chips, `kbd`/shortcut hints,
captions, code, icons, dense controls — take a **component-scoped (cropped) screenshot**
and read it close:

```
browser_take_screenshot({ target: '<selector for that element>', type: 'png', filename: '…' })
```

Read each glyph as a typographer would: font **family / size / weight / case**,
letter-spacing, optical padding & vertical centering, border/radius, truncation, icon
stroke weight — and whether it **matches the rest of the app's grammar** (find the
existing pattern via ministr before accepting a new one). **Rule: if you can't read the
text in the screenshot, you have not reviewed it — crop (or shrink the viewport) until you
can, then look again.**

### 2.4 Mechanical review — assert, don't vibe

Back every behavioral claim **and every mechanical slop claim** with a `browser_evaluate`
DOM assertion returning a small JSON verdict you can check. The reusable probe recipes
(console-error sweep, render-richness count, interactivity-survives, keyboard
`activeElement`, invoke-count cache probe, nav-convention / no-dead-ends,
controlled-input round-trip, persisted-state-after-flash, forced-colors emulation,
**typography computed-style audit**, the **anti-slop probes** (emoji-in-DOM scan,
off-scale-spacing / non-token-color scan, gradient-and-shadow census, placeholder-copy
scan, click-div / aria-echo honesty scan), and the **object-model fidelity probes** (§2.5))
live in **`reference/probes.md`** — read it when you need a probe rather than re-deriving
one. A probe that itself errors is a *probe* bug — fix the probe, don't report it as an app
regression.

### 2.5 Object-model fidelity — does the UI honor its objects? (OOUX)

The complement to anti-slop: slop asks *"does it look designed?"*; fidelity asks *"does it
honor the object model?"* (`/craft` §B0/§A6). Intuitive UX requires **recognizable,
connected, valuable objects** (OOUX manifesto #9) — so a view that's slop-free but renders
the same object two different ways, or scatters its actions, still fails. Check, per object
on the audited views:

- **One renderer, actually used** — every appearance of an object routes through its single
  canonical component. A second, drifting renderer is **dead code** (§A6): flag it.
- **Recognizable across zoom** — chip → row → card → detail are the *same thing* getting
  richer (persistent identity cues: icon, name, key color), not four unrelated designs.
- **CTA consistency** — the same object offers the same verbs, labeled identically, wherever
  it appears ("Sign" here vs "Add signature" there is one CTA with two names — a finding).
- **Relationships via shared children** — a parent object renders nested objects with the
  child's *own* component, and cardinality is visible (a 1:N link shows a list affordance).
- **Attribute completeness** — every content attribute has a home; empty/loading/error/
  partial states exist.

Back these with the **object-model fidelity probes** in `reference/probes.md` (one-renderer
census via `data-object` / shared testid, CTA-verb consistency across views, nested-child
component identity). A fidelity break blocks "done" like a slop finding; fix the bounded set
(§3) or file it as a backlog chunk with the object map (B0) attached.

---

## 3. Fix the bounded set, then re-verify

Apply the bounded set of fixes the audit surfaced — **slop findings included, treated as
blocking** — in the project's own grammar (don't sprawl into a redo; file larger findings
as backlog chunks). Re-run §1 + §2 on the changed targets, then the project's real gate
(`/craft` §A4) — typecheck + lint + the Storybook Vitest run + build — green.

## 4. Report

- What was audited (stories × themes), and which job found what.
- **Slop ledger (§0):** every slop tell found, with the screenshot/probe evidence and the
  in-grammar fix — and which were killed vs deferred. If you found none, say so *and* say
  what you checked for (don't imply absence you didn't verify).
- Visual findings (2.3) and which were fixed; mechanical evidence (2.4) **with the numbers**.
- Automated-gate status (Vitest addon + axe + slop probes green; any seeded-failure proof).
- Honest negatives — what's still weak / deferred (`/craft` §A5).
- Stop Storybook if you started it for a one-off.

## Discipline

- **Anti-slop is the prime directive, not a section.** It rides on every screenshot, every
  probe, every review. The default model output is slop; refuse it religiously.
- **Slop is divergence-toward-generic from the project's own grammar.** Ground in that
  grammar with ministr *before* judging; a new pattern is guilty until justified.
- **A slop finding blocks "done" like a failing test.** Renders + axe-clean + sloppy = not done.
- **Name the tell, cite the evidence, give the in-grammar fix.** "Feels generic" is not a finding.
- **Emoji-as-UI, gradients-for-no-reason, lorem/fake data, off-scale values, Title Case
  marketing copy** — these are near-always slop. Hunt them by default.
- **Object-model fidelity is a second gate (§2.5).** Slop-free isn't enough: the same
  object must be recognizable, render through one component, and carry consistent CTAs
  everywhere (OOUX manifesto #9). A drifting second renderer is dead code, not a variant.
- **Light AND dark, every time.** Half the regressions — and half the contrast/gradient slop — hide in the other theme.
- **Detail lives below frame resolution.** A full-frame shot verifies layout, never
  typography or emoji-icons — crop to the component (`target:` selector). **If you can't
  read it in the screenshot, you didn't review it.** Back it with the typography +
  anti-slop probes.
- **A claim without a probe is a guess.** "Keyboard nav works" / "no emoji in the UI" each need their assertion.
- **The addon gate ≠ done.** axe-passes-and-renders-and-lints is the floor; taste (the §0/2.3 read) is the rest, and it is not lintable.
- **Don't fixed-sleep for async renderers** — poll for the ready signal.
- **A probe error ≠ an app bug.** Re-check your probe before blaming the UI.
- This is `/craft` §B3–B5 made executable; the framing principles live there.

---

*Grounded in June-2026 norms (serpapi): Storybook *Writing tests* / *Vitest addon* /
*Accessibility tests* docs (stories→browser-mode tests + axe-on-every-story); the
anti-AI-slop doctrine (§0) as the prime directive across all facets; Object-Oriented UX /
ORCA (Sophia Prater, ooux.com) behind the §2.5 object-model-fidelity gate; Anthropic
skill-authoring (progressive disclosure → probe library in reference/). Last refreshed
2026-06-08.*
