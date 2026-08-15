# Sharing traces with your team

> **How-to** — steps for one task, assuming a working install ([all docs](README.md)).

think-and-ship can mirror its reasoning + execution traces **into your repo**
under `.think-and-ship/`, as a strict superset of the
[Agent Trace](https://agent-trace.dev/) standard — so the team accumulates
git-native AI Decision Records and generic Agent Trace tooling reads them too.
Full wire format: [SCHEMA.md](SCHEMA.md).

**1. Turn it on** (in your MCP config `env`, or the shell):

```sh
export THINK_AND_SHIP_SYNC_TARGET=repo-git   # mirror into <repo>/.think-and-ship/
export THINK_AND_SHIP_SHARED=true            # write to the committed partition
# optional: attribute code authorship to your model (models.dev convention)
export THINK_AND_SHIP_MODEL_ID=anthropic/claude-opus-4-8
```

Records stream to `<repo>/.think-and-ship/sessions/<session>.jsonl` and the
server makes **one commit per session** on close (`is_final_step` /
`ship_finalize`). With `THINK_AND_SHIP_SHARED` unset (the default), records go
to the **gitignored** `.think-and-ship/local/` partition instead — private
until you choose to share them.

**2. Install the redaction hook** — reasoning traces can contain pasted
secrets, so scan before committing:

```sh
git config core.hooksPath docs/deploy/hooks      # or symlink the single hook:
# ln -sf ../../docs/deploy/hooks/pre-commit .git/hooks/pre-commit
```

The hook runs [TruffleHog](https://github.com/trufflesecurity/trufflehog) (if
installed) plus a regex pass over staged `.think-and-ship/` files and blocks the
commit on a hit. Add project-specific patterns via
`THINK_AND_SHIP_REDACT_PATTERNS` (comma-separated regexes).

**3. Promote local scratch to the team** when a private record is worth
keeping:

```sh
think-and-ship trace promote --session my-project-a1b2c3 --step 7        # one think step
think-and-ship trace promote --session my-project-a1b2c3 --kind action   # all ship code actions
think-and-ship trace promote --session my-project-a1b2c3                 # whole session
```

`--step <n>` selects a think reasoning step by number; `--kind
<step|objective|task|action|check>` selects ship (or think) records by kind —
the two filters combine (AND). `promote` moves the matching records from
`local/` to `sessions/` (flipping `shared: true`) and leaves the rest
untouched. It doesn't commit — review the result, then `git add` + commit
(the hook scans on the way out).

> **Layout & rationale** — one file per session, one commit per session (never
> per step: at 100 sessions/day × 5 devs × 50 steps, per-step commits would be
> ~25k/day). See [ARCHITECTURE.md](ARCHITECTURE.md) →
> *Where a record can go*.
