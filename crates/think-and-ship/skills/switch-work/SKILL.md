---
name: switch-work
description: >-
  Choose which workstream you are working in, and in which mode. Run it when
  the user types the switch-work command, or says "switch to <workstream>",
  "focus billing", "work on authentication in build mode", "what workstream am
  I currently focused on?", or "which workstream am I in?". Sets a per-caller
  focus of
  {workstream, mode} where mode is shape, build or listen, then reports the
  frontier. With no arguments it reports the current focus and changes
  nothing. Selection only — it never writes code, never starts or completes a
  roadmap chunk, never processes a signal, and never reorders a plan. Use
  advance-work to actually do the work. Not for "fix this bug", "show roadmap
  status", "research X", or "implement the next chunk".
---

# switch-work

Point this caller at one workstream and one mode. Then stop.

You are choosing a *place to stand*, not doing work. Every verb that changes
code, chunk status or plan order belongs to `advance-work` or to a human.

## The state you are setting

```
{ project, lane, group, mode }
```

- **project** — resolved by the server from the working directory. Not yours to set.
- **lane** — *this caller*. See [Deriving your lane](#deriving-your-lane).
- **group** — the workstream. This is the roadmap's existing `group` field, the
  same one a tracker maps to a project. You are never inventing a taxonomy.
- **mode** — exactly one of `shape`, `build`, `listen`. No synonyms.

| Mode | What `advance-work` will then do |
|---|---|
| `shape` | One planning or research decision. Touches no implementation source. |
| `build` | One ready roadmap chunk, gated on the project's real checks. |
| `listen` | One stakeholder signal. |

## Deriving your lane

Focus is **per-caller**, so a lane is required and is never defaulted. Two
agents in two worktrees must not overwrite each other's focus, and a shared
default is exactly how that happens.

Use the first of these that you actually have, and use it consistently:

1. The absolute path of the repository or worktree root
   (`git rev-parse --show-toplevel`, else the working directory). **Prefer this** —
   it is stable across restarts, differs per worktree, and needs no bookkeeping.
2. A session or task id your harness gives you, if it survives restarts.

Do not invent a fresh lane per invocation: a lane that changes every turn can
never *hold* a focus. Do not use a constant like `default`, `main` or `agent` —
that is a shared slot wearing a per-caller name.

## Procedure

1. **Load state.** Call `roadmap_focus_get` with your lane. It is read-only and
   safe to call before you know whether a focus exists. If your harness defers
   MCP tool schemas, load the roadmap tools first.
2. **No arguments given?** Report the current focus and **stop**. Do not
   helpfully pick a workstream — see [Report only](#example-report-only).
3. **Resolve the workstream.** Pass what the user said to `roadmap_focus_set`
   as `group`. It accepts an exact name, a case-insensitive match, or an
   unambiguous fragment. Do not pre-guess or "correct" the user's wording; let
   the server resolve it so ambiguity is caught rather than papered over.
4. **Resolve the mode.** One of `shape` | `build` | `listen`. If the user did
   not say, ask — do not assume `build` because a repository looks ready.
5. **Handle refusal.** Unknown or ambiguous workstreams change nothing and come
   back with the exact candidates. Relay those candidates verbatim and stop.
   Never retry with a guess.
6. **Report.** Emit the report below, every field, every time.

## The report

Always these fields, in this order, whether you switched or only looked:

```
Focus switched
Lane: <lane>
Workstream: <group>
Mode: <shape|build|listen>
Ready: <count>
Blocked: <count>
Next: <chunk id, or none>
Persistent: <yes|no>
```

The first line is `Focus switched`, `Focus unchanged` or `Focus not set`.
`Persistent: no` means the server has persistence disabled and this focus dies
with the process — say so rather than letting the user discover it later.

## Boundaries

`switch-work` **must not**:

- Write, edit or refactor any file.
- Call `roadmap_start_chunk`, `roadmap_set_status`, `roadmap_complete_chunk`,
  or any other chunk-mutating verb. **Changing focus changes no chunk status.**
- Reorder, reprioritize, add or obsolete anything on the plan.
- Process, research or promote a signal.
- Change the lane it was given, or silently switch to a different lane.
- Invent a workstream that does not exist, or create one to satisfy a request.
- Continue into the work "while it's here". Stopping is the deliverable.

If the user asks for something outside this list, say which skill does it
(usually `advance-work`) and stop.

## Examples

### Positive — switch

> **User:** switch to authentication in build mode

```
Focus switched
Lane: /Users/dev/code/acme
Workstream: Authentication
Mode: build
Ready: 3
Blocked: 1
Next: auth-session-rotation
Persistent: yes
```

### Positive — an unambiguous fragment

> **User:** focus billing for shaping

`billing` matches only `Billing and invoicing`, so it resolves. The report
shows the **stored** name, not what was typed.

```
Focus switched
Lane: /Users/dev/code/acme
Workstream: Billing and invoicing
Mode: shape
Ready: 2
Blocked: 0
Next: billing-proration-rules
Persistent: yes
```

### Example: report only

> **User:** what workstream am I currently focused on?

No arguments, so nothing changes.

```
Focus unchanged
Lane: /Users/dev/code/acme
Workstream: Authentication
Mode: build
Ready: 3
Blocked: 1
Next: auth-session-rotation
Persistent: yes
```

### Negative — unknown workstream

> **User:** switch to payments

Nothing changed. The candidates come from the server, not from a guess:

```
Focus unchanged
No workstream matches 'payments'.
Workstreams in play: Authentication, Billing and invoicing, Platform
```

### Negative — ambiguous fragment

> **User:** switch to billing

```
Focus unchanged
'billing' is ambiguous — it matches Billing and invoicing, Billing reports.
Name one exactly.
```

### Blocked — a workstream nobody can move

> **User:** switch to platform in build mode

The switch itself succeeds. What is honest is the frontier:

```
Focus switched
Lane: /Users/dev/code/acme
Workstream: Platform
Mode: build
Ready: 0
Blocked: 4
Next: none
Persistent: yes
```

All four are blocked. Say so, name what they are waiting on, and do **not**
switch the user to a different workstream to find them something to do.

### No work — an empty workstream

> **User:** focus platform for listening

```
Focus switched
Lane: /Users/dev/code/acme
Workstream: Platform
Mode: listen
Ready: 0
Blocked: 0
Next: none
Persistent: yes
```

An empty workstream is a real answer. `advance-work` will return an honest
no-work receipt; it will not go looking elsewhere.

### Not this skill

These are **not** switch-work, even though they mention a workstream:

| The user says | Do this instead |
|---|---|
| "fix this authentication bug" | Just fix it, or use `advance-work` if it is a planned chunk |
| "show roadmap status" | Read the roadmap directly |
| "research payment providers" | `advance-work` in `shape` mode, once focused |
| "implement the next chunk" | `advance-work` |

## Failure and stopping

Stop and report — never work around it — when:

- **No lane can be derived.** Say so and ask for one. Do not substitute a
  constant.
- **The workstream is unknown or ambiguous.** Relay the candidates. Do not retry.
- **The mode is missing or not one of the three.** Ask. `implement`, `code` and
  `plan` are not synonyms for anything.
- **The roadmap tools are unavailable.** Report that focus cannot be set, and do
  not simulate it in conversation — a focus the server does not hold is not one.
- **No workstream exists at all.** Report it plainly; grouping the roadmap is a
  separate, human-led decision.

Every one of these ends with the report block and no mutation.
