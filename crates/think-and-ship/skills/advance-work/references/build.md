# build mode

Complete **one** ready roadmap chunk inside the focused workstream, behind the
project's real gates. Then stop.

## Select the chunk

Take the focused frontier's `next`. It is the most urgent `pending` chunk (smallest `priority` number) in
this workstream that carries no blocker and whose dependencies are all done.

If `next` is empty, emit a `no-ready-work` receipt. **Do not** take a chunk from
another workstream, and do not promote a backlog chunk to manufacture work —
promotion is a planning decision, which is shape's job or a human's.

If you deviate from `next`, name the reason in the receipt.

## The sequence

1. **Start the chunk.** Mark it in progress and take the `chunk:<id>` backref.
2. **Set a ship objective** from the chunk's **real acceptance criteria** —
   the ones stored on the chunk, plus any requirement ids from the spec. Do not
   paraphrase them into something easier to satisfy; the objective is what the
   final report is audited against.
3. **Open the reasoning trace.** Mandatory, before implementing. Record the
   sub-plan and what you expect to be hard. Cross-reference it to the chunk.
4. **Ground in the existing implementation.** Find the code this touches and
   read it. Check who calls what you are about to change *before* changing it.
   Use a code-intelligence MCP server if present; otherwise your harness's
   ordinary search and read tools.
5. **Use Spec Kit artifacts automatically** when `.specify/` and a matching
   feature exist — see [speckit.md](speckit.md).
6. **Implement.** Match the surrounding code's idiom. Keep the change inside the
   chunk's scope; discoveries outside it go to the backlog, not into this diff.
7. **Verify** — below.
8. **Complete ship and roadmap state**, but only once required gates pass.
9. **Close and link the reasoning trace**: what shipped, what deviated, and the
   honest negative findings.
10. **Recompute** the next candidate without executing it. Emit the receipt.

## Verify

Run the project's **real** commands — the ones its CI runs, exactly as written.
Not a subset you believe is equivalent, and not a faster proxy.

Prefer recording each check by handing the server the command to run, so the
real exit code is captured. A check whose result you typed in yourself is
unverified, and the ship report will say so.

Also run, where they exist and are relevant to the chunk:

- The scenario or quickstart that proves this unit's behaviour.
- Contract or interface tests for any boundary the chunk touched.
- The project's constitution or consistency checks.

### A red gate means the chunk is not done

Do not finalize. Do not mark the chunk complete. Do not describe it as "done
except for". Fix and re-run, or emit a `blocked` receipt naming the failing
check and its exit code.

**A skipped check is red.** If a required gate could not run, say so explicitly
and treat completion as refused. "I could not run the tests, but the change is
small" is exactly the sentence this rule exists to prevent.

### Piping hides failures

Record gates as bare commands. Appending a pager or filter to a gate makes the
recorded exit code that of the filter, so a failing gate is recorded as passing.
Read the output separately if you need to.

## Scope discipline

The chunk is the unit. While implementing you will find other things wrong —
that is normal and valuable. Put them in the backlog with what you observed, and
leave them. A diff that fixes four unrelated things cannot be reviewed, and its
gates cannot tell you which change broke what.

## What "done" looks like

```
Focus: Authentication
Lane: /Users/dev/code/acme
Mode: build
Unit: auth-session-rotation — Rotate session tokens on privilege change
Result: completed
Evidence:
  - cargo test --workspace --all-targets: exit 0
  - cargo clippy --workspace --all-targets -- -D warnings: exit 0
  - cargo fmt --all -- --check: exit 0
  - quickstart scenario 3 (privilege escalation re-issues the session): passed
Native records:
  - chunk:auth-session-rotation (done)
  - task:implement-rotation, task:verify-rotation
  - think:418 (open), think:421 (close, pinned)
  - check:cargo-test, check:cargo-clippy, check:cargo-fmt
Discoveries:
  - the refresh token outlives rotation; filed as a backlog chunk
Next candidate: auth-device-binding
Stop reason: one-unit boundary
```

The discovery is filed, not fixed. That is the boundary working.
