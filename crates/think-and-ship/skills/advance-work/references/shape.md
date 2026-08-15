# shape mode

Complete **one** bounded planning or research decision inside the focused
workstream. Then stop.

Shape exists so that thinking is a first-class unit of work rather than
something smuggled into an implementation task. Its whole value is that it
produces a *decision*, recorded, that a later build does not have to re-derive.

## The one hard boundary

**Shape must not modify implementation source.**

Concretely, do not edit application or library code, tests, build
configuration, or anything the project ships. If you find yourself opening an
editor on a source file to "just try it", you have left shape — stop, and say
in the receipt that the question needs a build unit instead.

What you *may* write:

- Specification, plan, research and requirement documents.
- Roadmap units: add, split, update, obsolete, or **propose** reprioritization.
- Reasoning-trace steps recording the decision or the negative finding.
- Signal enrichment, where the decision resolves a signal.

A prototype is not exempt. If a question can only be settled by running code,
the honest answer is a `blocked`/`awaiting-human` receipt naming that fact, or a
build unit created for it — not an unrecorded code change under a research
label.

## Eligible units

Exactly one of:

| Unit | Done when |
|---|---|
| An unresolved design question | A decision is recorded, with the evidence and the rejected alternatives |
| An ambiguous requirement | The ambiguity is resolved in the requirement document, or escalated |
| One coherent area of a spec or plan | That area is amended and internally consistent |
| A roadmap unit that is wrong | It is updated, split, or obsoleted with a reason |
| A validated negative finding | It is recorded so nobody re-investigates it |

"One coherent area" means a section, not a document. Rewriting the plan is
several units.

## Procedure

1. Open a reasoning-trace step framing the question and what would settle it.
2. Ground it. Read the actual code and the actual current documents before
   researching the outside world — most "open questions" are already answered
   somewhere in the repository, and finding that out is a cheaper result.
3. Research externally only if grounding did not settle it. Use a web-search
   MCP server if present, otherwise your harness's ordinary web capability. If
   neither exists, say so and reason from what you have.
4. Decide, or record honestly that it cannot be decided yet and what is missing.
5. Write the decision where it belongs — the spec, the plan, the roadmap unit,
   or a pinned reasoning step. A decision that lives only in the receipt is lost.
6. Close the reasoning step and emit the receipt.

## Reprioritization is a proposal

If the finding implies the plan's order is wrong, record a **proposal** and stop.
Do not accept it on the user's behalf, even when it looks obvious — order
expresses what matters to a person, and this mode does not have that
information. The receipt reports `awaiting-human` when a proposal is the unit's
main output.

## Negative findings are results

"We investigated X and it does not work, because Y" is a complete unit. Record
it with the same care as a positive decision. An unrecorded negative finding is
re-investigated by the next session at full price.

## What "done" looks like

```
Focus: Billing and invoicing
Lane: /Users/dev/code/acme
Mode: shape
Unit: billing-proration-rules — Decide proration behaviour on mid-cycle downgrade
Result: completed
Evidence:
  - src/billing/cycle.rs already implements upgrade proration; downgrade is unhandled
  - two providers' published behaviour compared; both credit forward rather than refund
Native records:
  - chunk:billing-proration-rules (acceptance updated)
  - think:412 (pinned — decision + rejected alternative)
Discoveries:
  - the refund path assumed by the original chunk does not exist in the code
Next candidate: billing-dunning-copy
Stop reason: one-unit boundary
```

Note what is absent: no source file was touched, and the decision landed in the
chunk's acceptance criteria and a pinned reasoning step — the two places a later
build unit will actually read.
