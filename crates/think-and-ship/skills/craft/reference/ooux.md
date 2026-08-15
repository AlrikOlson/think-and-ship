# OOUX — object-first design, mapped to Storybook

The detail behind `/craft` §B0 (and §A6). Loaded on demand. OOUX = **Object-Oriented
UX** (Sophia Prater); **ORCA** = its process — **O**bjects → **R**elationships →
**C**TAs → **A**ttributes. The thesis that makes it a *craft* practice, not a workshop:

> **Objects are the unit of UX, and Storybook is the object model made visible.**
> §B1 builds *bottom-up* (tokens → atoms → components). OOUX adds the *top-down*
> counterpart (domain objects → their representation → CTAs/attributes/relationships).
> They meet in Storybook: it gets **organized by object**.

Keep it **lite**: an object map is a few minutes in a `think` step, not a deliverable.
Just-enough up-front structure, then iterate (OOUX manifesto #2/#8) — never big-design-
up-front, never no-design.

---

## The ORCA-lite pass (per new surface)

Ground every step in the real domain with `ministr` — reuse the code/schema's nouns and
verbs; do **not** invent a parallel vocabulary.

1. **Objects** — list the system's core nouns (the things users point at, value, and
   return to). Prune to the load-bearing few — kill darlings (manifesto #12). A good
   object is *recognizable, connected, valuable* (manifesto #9); a screen, a tab, or a
   CRUD form is **not** an object.
2. **Relationships** — for each pair, the link + **cardinality** (1:1, 1:N, N:M):
   "a Catalog *contains many* Blocks", "a Block *requires many* Capabilities". Nesting +
   cardinality decide navigation and which objects render *inside* which.
3. **CTAs** — the verbs the user can apply to each object (Create / Sign / Diff / Apply /
   Inspect…). These are the object's affordances and map to real commands/routes. A CTA
   with no object, or an object with no CTA, is a smell — re-examine.
4. **Attributes** — the object's **content** fields (shown to the user) + **metadata**
   (system fields). These become props and the variant axes of the object's component.

**Output — the object map** (one screen, in a `think` step):

```
OBJECT            ATTRIBUTES (content · meta)          CTAs              NESTS
Block             id@semver · capabilities · hash      Inspect · Sign    Capability, Port
Capability        id · schema · constraints            —                 Constraint
Plan              actions[] · target · createdAt       Render · Apply    Action
```

It exposes complexity **early** (cheap) instead of mid-build (expensive) — the §A8 spec
discipline applied to information architecture.

---

## Object ↔ Storybook ↔ code mapping

This is the integration. Each ORCA facet has a home in Storybook and in code:

| ORCA facet | Storybook | Code (§A6) |
|---|---|---|
| **Object** | a **top-level section** (`Objects/Block`) | a module/type — **one renderer** |
| **Object's appearances** | stories at each **zoom**: `Chip` · `Row` · `Card` · `Detail` | one component, a `mode`/`zoom` prop — *not* N components |
| **Attributes** | **controls / variant** stories + the empty·loading·error states (attribute-completeness) | props; the variant axes |
| **CTAs** | **interaction stories** (`play` fn) exercising each verb | the actions/commands/routes on the object |
| **Relationships** | **composition stories** — a parent renders its children via the **same child components** | typed links; nested components, no bespoke re-render |
| *(tokens/atoms)* | a **Foundations** section *beneath* the objects | the design-token + primitive layer (§B1) |

Concretely, the Storybook tree reads:

```
Objects/
  Block/          Chip · Row · Card · Detail · WithIllegalWire(play) · Empty · Loading · Error
  Plan/           Summary · Detail · Apply(play) · …
  Capability/     …
Foundations/      Tokens · Button · Badge · Field · …
```

The **Nested Object Matrix** literally becomes nested stories: `Plan/Detail` renders
`Action` rows using the *same* `Action` component the `Action` section documents. If a
parent re-implements a child's look, that's a fidelity break (below), not a shortcut.

---

## Object-model fidelity — the bar gui-scrutiny verifies (§2.5)

Beyond anti-slop, a GUI honors its object model iff:

- **One renderer per object, actually used.** Every appearance of an object routes
  through its canonical component (`data-object="block"` or equivalent). A second,
  drifting renderer is **dead code** (§A6) — delete it.
- **Recognizable across zoom.** Chip → Row → Card → Detail are the *same object* getting
  richer, not four unrelated designs. Identity cues (icon, name, key color) persist.
- **CTA consistency.** The same object offers the same verbs, labeled the same way,
  wherever it appears. "Sign" here and "Add signature" there is one CTA with two names —
  unify.
- **Relationships render through shared children.** Nested objects use the child's own
  component; cardinality is visible (a 1:N relationship shows a list affordance, not a
  single inline blob).
- **Attribute completeness.** Every content attribute has a place, and the empty /
  loading / error / partial states exist as stories.

These map to the probes in `gui-scrutiny/reference/probes.md` → *Object-model fidelity*.

---

## The OOUX manifesto values that are load-bearing here

Not the whole manifesto — the parts that change what we *do*:

- **#9 Respect the human brain** — intuitive UX needs *recognizable, connected, valuable
  objects*. This is the whole reason for object-first + the fidelity bar.
- **#3 / #14 Complexity tackled early / head-on** — the object map surfaces complexity
  before expensive late pivots.
- **#2 / #8 Just-enough up-front + strategic iteration** — ORCA-lite is minutes, then we
  build and iterate. We design *solid foundations and reusable parts* (one renderer per
  object), not feature-by-feature reinvention.
- **#12 Clear scope / kill darlings** — prune the object set; don't model everything.
- **#11 Long-term, resilient IA** — objects outlive screens; structuring by object is the
  future-proof cut.

---

*Source: Object-Oriented UX & the ORCA methodology — Sophia Prater / Rewired
(ooux.com), incl. the OOUXer Manifesto. Integrated into `/craft` §A6 + §B0/§B2/§B5 and
`/gui-scrutiny` §2.5. Added 2026-06-08.*
