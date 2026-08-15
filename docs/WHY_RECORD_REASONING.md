# Why record reasoning at all

> **Explanation** — background and reasoning; nothing here is needed to operate the tool ([all docs](README.md)).

An agent's reasoning dies with the conversation that produced it. The diff
survives; the reason for the diff does not. This document is the argument
for making that reasoning a durable record — the claim the whole tool rests
on. If you just want to use it, start with the [tutorial](TUTORIAL.md).

## The failure this addresses

A coding agent works inside a context window, and everything it knows that
is not written to disk vanishes when the session ends. Three concrete
losses follow:

**Decisions get re-derived or reversed.** The next session faces the same
question — which retry strategy, which auth model, why this dependency —
with none of the deliberation that answered it last time. Sometimes it
spends the tokens to re-derive the same answer. Sometimes it quietly picks
the other one, and the codebase now disagrees with itself.

**Review loses its evidence.** A reviewer reading yesterday's agent change
cannot tell a deliberate tradeoff from an accident. The diff shows a
timeout of 30 seconds; nothing shows whether 30 was measured, copied, or
guessed.

**Verification claims are testimony.** An agent that reports "all tests
pass" is a witness, not a record. If it misread the output — or never ran
the suite — the claim and the truth diverge with nothing to catch them.

## Why written records, specifically

Human engineering already solved this problem once, for humans:
Architecture Decision Records write down a decision, its context, and its
consequences at the moment they are freshest, precisely because the team
that made the decision will not be in the room when it is questioned. An
agent is the limit case of that team — it is *never* in the room again. A
`think_record_step` is an ADR at conversation granularity: purpose,
context, the reasoning itself, the outcome, and what it depended on.

Recording changes the reasoning as well as preserving it. A hypothesis
written as a step with a confidence value and named dependencies can later
be revised, refuted, or pinned — and anything built on a refuted step is
findable. Reasoning held only in the context window can do none of that.

## Why the execution side is recorded too

Reasoning alone would be a diary. The `ship_*` family ties each thought to
what actually happened: a step names the task it motivated
(`execution_ref: "task:tracker-retry"`), an action names the step that
motivated it, and a quality gate recorded with a `command` is run by the
server itself, which stores the real exit code. That last mechanism is the
honesty floor: a `verified: true` check cannot be produced by an agent
narrating success, only by the command actually exiting 0.

The result is one graph — plan (`roadmap_*`), reasoning (`think_*`), work
(`ship_*`), and stakeholder input (`signal_*`) — filed under a project id
derived from the working directory. Six months later it answers the
question the diff cannot: not what changed, but why, on whose evidence,
and whether the gates were actually green.

## The honest cost

Recording is overhead: a step costs tokens to write, and an agent under
instruction to record will sometimes record the trivial along with the
load-bearing. The records pay only when they are read — by a later
session recalling a decision, a reviewer auditing one, or a teammate
reading a [shared trace](SHARED_TRACES.md). On a throwaway script the
overhead buys nothing. The tool's bet is narrower than "always record":
it is that on any project that outlives its conversations, the reasoning
is the most expensive thing currently being thrown away.
