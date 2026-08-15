# Migrating the agent skills

> **How-to** — steps for one task, assuming a working install ([all docs](README.md)).

Two things changed for anyone who ran `skills install` before Iteration 3.

## 1. The Codex destination moved

This installer used to write `~/.codex/skills`. **No first-party OpenAI page
lists that path.** Codex reads `~/.agents/skills` and `$CWD/.agents/skills`
(verified 2026-08-08, see [HARNESSES.md](HARNESSES.md)).

Leaving the old tree in place is not harmless. Cursor's documented legacy
compatibility list includes `~/.codex/skills`, so a stale copy there is still
**discovered** — and you would be running the previous version's skill with
nothing telling you which one answered.

```sh
think-and-ship skills migrate            # preview; writes and removes nothing
think-and-ship skills migrate --apply    # remove only what it can prove is safe
```

`migrate` removes a directory **only** when it is byte-identical to what this
binary renders. Anything else is reported and kept, because a local edit and an
older version's copy are indistinguishable from the filesystem — there is no
metadata separating them, and deleting a customization is not recoverable. If
you want a differing directory gone, remove it yourself or re-run with
`--apply --force`.

Then install to the current destination:

```sh
think-and-ship skills install --client codex
```

## 2. The default surface is two skills

`skills install` now writes the **core profile** — `switch-work` and
`advance-work` — and nothing else. A catalog is a budget: every skill an agent
discovers costs metadata in the model's context at startup, and eleven
overlapping workflow skills spent that budget on choosing between them.

Nothing was deleted. The older skills still ship inside the binary:

```sh
think-and-ship skills install --profile legacy   # the pre-Iteration-3 set
think-and-ship skills install --profile all      # both profiles
think-and-ship skills install --only roadmap     # one, by name
```

Where each one went:

| Was | Now |
|---|---|
| `/roadmap`, `/roadmap-sk` | `advance-work` in `build` mode (Spec Kit is an automatic adapter) |
| `/roadmap-refresh`, `/roadmap-refresh-sk` | `advance-work` in `shape` mode |
| `/roadmap-run` | Repeated `advance-work` calls — one unit each, by design |
| `/signals` | `advance-work` in `listen` mode |
| `/handoff` | The `advance-work` receipt |
| `/craft` | Internal doctrine the core skills apply |
| `/business-intel`, `/gui-blueprint`, `/gui-scrutiny` | **Not superseded.** Optional specialists; install them by name |

Skills already installed keep working — they are left exactly where they are.
`skills migrate` reports them and removes none of them.

## Known limitations

- **Cline, Roo Code, Amp, Goose and Kiro document no manual-only invocation
  control.** `advance-work` writes code, so on those agents the mitigation is
  in the skill text — a narrow description and a first-step explicit-invocation
  guard. That is a mitigation, not a vendor control, and it is weaker.
- **Windsurf** is the one case with a documented alternative: its own guidance
  makes Workflows manual-only where Skills are not, so the installer also writes
  a thin workflow that points at the skill.
- **Runtime is unexercised for most agents.** The packages are validated —
  official validator, parseable frontmatter, only documented keys, byte-identical
  bodies, correct destinations — but only Claude Code was exercised as a running
  agent here. For the other eleven the honest claim is *artifact validated,
  runtime not exercised*.
