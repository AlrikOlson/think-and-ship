# The receipt

Every `advance-work` invocation ends with one — whether it completed a unit,
refused to, or found nothing to do. The receipt is how a person reviews a unit
without re-deriving it, so a missing field is a defect even when the work was fine.

## Shape

```
Focus: <group>
Lane: <lane>
Mode: <shape|build|listen>
Unit: <stable id and title>
Result: <completed|blocked|no-ready-work|awaiting-human>
Evidence:
  - <check, artifact, or source reference>
Native records:
  - <chunk:/task:/think:/signal:/check: refs>
Discoveries:
  - <new facts or "none">
Next candidate: <id/title or none>
Stop reason: one-unit boundary
```

**All ten fields, every time.** Where there is nothing to say, say `none` — an
omitted field reads as an oversight, and `none` is a claim.

## Field rules

**Focus / Lane / Mode** — as loaded at step 1, not as you wish they were. If
they are wrong, that is a stop, not a correction.

**Unit** — a stable id *and* a human title. An id alone is unreadable in six
weeks; a title alone cannot be looked up.

**Result** — one of exactly four:

| Result | Means |
|---|---|
| `completed` | The unit is done and its required checks passed |
| `blocked` | It was started and cannot finish — say what stopped it |
| `no-ready-work` | Nothing was eligible in this workstream and mode |
| `awaiting-human` | Progress needs a decision that is not yours to make |

`completed` is a claim about evidence, not about effort.

**Evidence** — what would let someone else believe the result:

- For a check: the command **and** its exit code or verdict. `cargo test: exit 0`,
  not "tests pass".
- For a source claim: the file or symbol you actually read.
- For research: what was consulted.
- For a reduced surface: say it. "No code-intelligence server available; grounded
  by reading src/billing/renew.rs directly" is a good evidence line. Silence
  about a missing tool reads as grounding that never happened.

Never list a check you did not run. Never list one you ran and ignored.

**Native records** — the cross-references that make this unit findable later:
chunk, task, think step, signal, check. If you recorded nothing, the unit
probably was not real.

**Discoveries** — things now known that were not known before, including
negative findings and anything filed to the backlog. `none` is a legitimate and
common answer; padding it is worse than leaving it empty.

**Next candidate** — recomputed, **not executed**. This is the field that makes
the one-unit boundary usable: the user can see what is next and decide whether
to run again. Naming it is not permission to do it.

**Stop reason** — normally `one-unit boundary`. When something else stopped you,
say that instead, specifically: `required check failed — completion refused`,
`no focus set — run switch-work first`, `awaiting a product decision`.

## Worked examples

### Completed

```
Focus: Authentication
Lane: /Users/dev/code/acme
Mode: build
Unit: auth-session-rotation — Rotate session tokens on privilege change
Result: completed
Evidence:
  - cargo test --workspace --all-targets: exit 0
  - cargo clippy --workspace --all-targets -- -D warnings: exit 0
  - quickstart scenario 3 (privilege escalation re-issues the session): passed
Native records:
  - chunk:auth-session-rotation (done)
  - task:verify-rotation, think:421, check:cargo-test
Discoveries:
  - the refresh token outlives rotation; filed as a backlog chunk
Next candidate: auth-device-binding
Stop reason: one-unit boundary
```

### Blocked

```
Focus: Authentication
Lane: /Users/dev/code/acme
Mode: build
Unit: auth-session-rotation — Rotate session tokens on privilege change
Result: blocked
Evidence:
  - cargo test --workspace: FAILED (exit 101), 2 tests in session::rotate
Native records:
  - chunk:auth-session-rotation (still in_progress)
  - check:cargo-test (failed, required)
Discoveries:
  - rotation invalidates the refresh token, which the spec did not anticipate
Next candidate: auth-session-rotation (same unit, after the failure is fixed)
Stop reason: required check failed — completion refused
```

Note the chunk is still in progress. A blocked unit must not leave the plan
claiming it shipped.

### No ready work

```
Focus: Platform
Lane: /Users/dev/code/acme
Mode: build
Unit: none
Result: no-ready-work
Evidence:
  - focused frontier: ready 0, blocked 4
  - platform-tls-rotation blocked on an external certificate authority
Native records:
  - none
Discoveries:
  - none
Next candidate: none
Stop reason: one-unit boundary
```

### Awaiting human

```
Focus: Billing and invoicing
Lane: /Users/dev/code/acme
Mode: shape
Unit: billing-dunning-policy — Decide retry cadence for failed payments
Result: awaiting-human
Evidence:
  - three cadences compared; all three are defensible on the evidence available
  - the choice trades recovered revenue against customer irritation
Native records:
  - chunk:billing-dunning-policy (reprioritization proposal recorded)
  - think:433 (pinned — the three options and their costs)
Discoveries:
  - the current implementation retries once, which none of the three matches
Next candidate: billing-proration-rules
Stop reason: awaiting a product decision
```

The options were narrowed and recorded; the choice was left to the person whose
call it is. That is a completed unit of shaping, not a failure to decide.
