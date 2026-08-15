# listen mode

Process **one** stakeholder signal relevant to the focused workstream. Then
stop.

A signal is something a person raised: a question, an idea, a concern, a bug
report, a piece of feedback. Listen turns one of them from a raw remark into a
recorded, grounded, dispositioned item.

## Select exactly one signal

Take an unresolved signal already associated with the focused workstream.

If none is associated, you may take one whose relevance to this workstream you
can **establish with evidence** — name the code, the chunk, or the product area
that connects them, in the receipt. A signal that merely sounds related is not
related; guessing here quietly re-files other people's work into the wrong
workstream.

If neither exists, emit a `no-work` receipt. Do not process a signal belonging
to a different workstream, and do not go and find new signals to widen the pool.

**Never process a second signal in the same invocation.** Not even a trivial
one, not even one that "obviously" resolves alongside the first.

## Procedure

1. **Read the signal as written.** What did the person actually say, and what
   did they actually ask for? Do not restate it into something more convenient
   to answer.
2. **Ground it in the code and the current product state.** Is it already true?
   Already fixed? Already impossible? This is the step that most often resolves
   a signal outright, and it is cheap.
3. **Research externally only if grounding left a real question** — a claim
   about another product, a standard, a practice. Use a web-search MCP server if
   present; otherwise your harness's ordinary web capability; otherwise say so.
4. **Record the enrichment** on the signal itself: what you found, how confident
   you are, and what you looked at. The enrichment is the durable output — the
   disposition below is just the consequence of it.
5. **Disposition it** using the project's existing signal lifecycle:

   | Disposition | When |
   |---|---|
   | Promote | It is validated and worth doing → becomes a roadmap unit |
   | Dismiss | It is answered, already true, or out of scope — with the reason |
   | Defer | It is real but not now — with what would make it ready |
   | Surface | It needs a human decision → raise it, do not decide it |

6. Emit the receipt.

## Surfacing is not failing

Raising something to a human is a completed unit when the decision is genuinely
theirs — a product tradeoff, a priority call, a promise to a customer. Record
what you learned so the human is deciding on evidence rather than on the raw
remark. Interrupt for what earns it; do not surface every signal to be safe.

## No relevant signal

An honest empty receipt:

```
Focus: Billing and invoicing
Lane: /Users/dev/code/acme
Mode: listen
Unit: none
Result: no-ready-work
Evidence:
  - 3 unresolved signals pending; none associated with this workstream
  - checked the 3 for relevance: two concern authentication, one concerns CI
Native records:
  - none
Discoveries:
  - none
Next candidate: none
Stop reason: one-unit boundary
```

Note that it says what was checked. "No relevant signal" without that is
indistinguishable from not having looked.

## What "done" looks like

```
Focus: Billing and invoicing
Lane: /Users/dev/code/acme
Mode: listen
Unit: signal:412 — "Why did my annual plan renew at the old price?"
Result: completed
Evidence:
  - src/billing/renew.rs reads the price at subscription time, not at renewal
  - the behaviour is intended and documented, but the renewal email omits it
Native records:
  - signal:412 (enriched, promoted)
  - chunk:billing-renewal-email-price (new, backlog)
  - think:430
Discoveries:
  - the report is not a pricing bug; it is a communication gap in one email
Next candidate: signal:418
Stop reason: one-unit boundary
```

The grounding changed the answer: what arrived as a billing bug turned out to be
a copy problem, and the promoted chunk reflects the real cause rather than the
reported one.
