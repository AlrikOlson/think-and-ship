# Documentation style

> **Reference** — facts to look up, not a reading path ([all docs](README.md)).

This file is the project's documentation voice, written as rules. A reviewer
rejects a paragraph by citing a rule number ("violates S2"); a writer defends
one the same way. Anything this file does not govern is listed at the end,
so a rejection outside that boundary is itself out of order.

Every BEFORE below is a real quote from this repository's prose, cited by
file. The BEFOREs intentionally break the rules; do not fix them where they
stand unless the surrounding document is being rewritten anyway.

## The one decision: layer, don't average

Terseness and beginner legibility pull against each other. A beginner needs
a definition the expert finds obvious; the expert reads the definition as
noise. This project does not average the two into a middle register. It
layers:

1. **The claim comes first and stands alone.** One terse sentence an expert
   can act on without reading further.
2. **The expansion follows immediately and is skippable.** One sentence —
   a parenthetical or the next sentence, never a separate section — that
   gives the beginner the term or the mechanism the claim spent.
3. **A term is defined exactly once, at first use, in the paragraph that
   spends it.** Later uses assume it. A document never re-defines a term to
   be safe; it links back instead.

Every rule below is applied inside that frame: the terse layer obeys the
rules, and the expansion layer is where a definition is allowed to live.

## Rules

### S1. Lead with the fact

The first sentence of a section carries the section's one actionable claim.
Framing, motivation, and rhetoric come after it or not at all.

**BEFORE** (README.md, "When a zero means something"):

> A count of 0 has two causes, and the number cannot tell them apart: the
> verb is genuinely unused, or nobody has run the workflow that uses it yet.

**AFTER:**

> The soak threshold is 500 calls and 14 active days — both, not either.
> Until it is met, `calls` reports a zero as "NOT EVIDENCE", because a zero
> cannot say whether a verb is unused or simply never had the chance.

### S2. One claim per sentence

A sentence carrying three clauses and two asides parses as work. Split it.
The claims lose nothing by standing apart.

**BEFORE** (README.md, "OpenTelemetry export"):

> That is the variable the OpenTelemetry exporter specification already
> defines, so it is the one your vendor's own onboarding page tells you to
> set; values are percent-decoded per the spec's W3C Baggage form, which is
> how a Basic credential's space survives.

**AFTER:**

> `OTEL_EXPORTER_OTLP_HEADERS` is defined by the OpenTelemetry exporter
> specification, so your vendor's onboarding page already tells you to set
> it. Values are percent-decoded per the spec's W3C Baggage form. That
> decoding is what lets a `Basic` credential's space survive.

### S3. Name the thing

"Project identity", "the wiring point", "the engine" are placeholders, not
names. Use the identifier a reader can search for: the environment variable,
the function, the file, the tool.

**BEFORE** (README.md, opening):

> Cross-references auto-correlate by project identity, giving you a full
> audit trail from "what the agent thought" to "what shipped."

**AFTER:**

> All four families derive one project id from the working directory, so a
> `think:12` reference recorded by `ship_record` resolves to the right trace
> without configuration.

### S4. Say what it does before why it is right

A reader who does not yet know what the feature does cannot evaluate the
argument for it. The capability sentence precedes the justification
sentence, every time.

**BEFORE** (README.md, "OpenTelemetry export" opening):

> Agent observability converged on the OTel GenAI semantic conventions, and
> think-and-ship speaks them natively: `trace export` maps your workspace
> onto agent spans […]

**AFTER:**

> `trace export` writes the workspace as one OpenTelemetry trace: the ship
> cycle as an `objective → task → action/check` span tree, think steps as
> reasoning spans. It follows the OTel GenAI semantic conventions, so any
> OTLP backend renders it without translation.

### S5. Anchor every quantity

"Fewer", "higher-confidence", "fast", "large" are claims without a measure.
Give the number, or name the mechanism that decides.

**BEFORE** (README.md, "Signal-driven development"):

> Surfacing follows an **earned-interruption** discipline (fewer,
> higher-confidence interruptions, never nagging) […]

**AFTER:**

> `signal_pending` returns a signal only when its status is `researched`,
> its best enrichment confidence clears the `min_confidence` you pass, and
> it is not snoozed. An unresearched signal scores 0.0 and can never clear
> a positive threshold.

### S6. Status claims carry a version or a date

"Not yet implemented", "coming soon", "currently" are true the day they are
written and silently false afterward. Pin every status to a version or a
date so the reader can tell a live claim from a fossil.

**BEFORE** (docs/ARCHITECTURE.md, opening — written 2026-05-27, still there
2026-08-12):

> The merge has not yet been implemented.

**AFTER:**

> This document describes the server that runs. Every structural claim was
> checked against the source on 2026-08-12.

The second version survives contact with time because it says *when* it was
true. The first was a design contract for a design that lost — the trait it
documented as the extension point never dispatched a call in production — and
nothing in three months of green tests said so. A status with no date cannot
be audited, so it never is.

### S7. Never spend a term you have not given

Every term of art costs the reader who lacks it. Spend it only after the
expansion layer has paid for it — one clause is usually enough. This is the
rule that decides whether a beginner stays.

**BEFORE** (README.md, opening — three terms spent, none given):

> One MCP server. Four tool families.

**AFTER:**

> One MCP server — a process an AI agent calls tools on over the Model
> Context Protocol. It serves four families of tools, each a prefix
> (`think_*`, `ship_*`, `roadmap_*`, `signal_*`) and a concern.

### S8. No process narration in reader-facing text

Phase numbers, saga names, chunk ids, and step references narrate the
project's development to people who were not there. They carry no meaning
outside the internal trace. This rule covers prose, source comments, and
commit messages alike.

**BEFORE** (README.md, "Signal-driven development"):

> Direct collaborator submission — webhook, GitHub Issues, inbound email, a
> public form — is the **Phase-30 cloud backend** […]

**AFTER:**

> Direct collaborator submission — webhook, GitHub Issues, inbound email, a
> public form — goes through the per-tenant cloud service
> (`docs/SIGNAL_CONTRACT.md`); inbound email ingress is live (2026-07), the
> other paths are still on the roadmap, and the local store becomes a cache
> of the cloud system-of-record.

Commit messages are enforced by `.githooks/commit-msg` (install with
`just hooks`). Commits from before the hook existed (2026-07-29) keep their
subjects as-is — a line in the sand, decided rather than defaulted: the
repository was private for all of them, rewriting history would re-point
every released tag and invalidate every `fixed in <sha>` reference in
source comments, and the inconsistency between old and new messages is
deliberate.

## What this file does not govern

- **Source comments and rustdoc bodies**, except S8 — code commentary has
  its own conventions (see the crate-level docs and clippy configuration).
- **CLI output and error message text** — those are interface, tested by
  the suite, and worded for the terminal, not the page.
- **Skill files under `crates/*/skills/`** — operator instructions for
  agents, written in an imperative register this guide does not describe.
- **The internal roadmap, think, ship, and signal state** — working notes,
  not publications.
- **The BEFORE quotes above** — they violate the rules on purpose and are
  kept verbatim as evidence.

A style complaint about any surface on this list cannot cite this file.
