# Spec Kit adapter

This is an **adapter**, not a separate skill. When a project uses Spec Kit, its
artifacts become the source of requirements and the definition of done for the
unit you are about to do. Nothing else about `advance-work` changes.

## Detection

```
test -d .specify
```

- **Absent** → this project does not use Spec Kit. Proceed on native roadmap
  state. **Do not create a `.specify/` tree, and do not initialize Spec Kit.**
  Adopting a methodology is a human decision, not a side effect of advancing work.
- **Present** → resolve its configuration below, then use its artifacts.

## Resolve configuration from the project, never from memory

Four things, each read from the repository:

| What | Where |
|---|---|
| Script flavour | `.specify/init-options.json`, key `script` — picks the scripts directory and the prerequisite probe |
| Command separator | `.specify/integration.json`, `invoke_separator` — `-` or `.`, so commands are `/speckit-plan` **or** `/speckit.plan` |
| Feature numbering | `.specify/init-options.json`, key `feature_numbering` |
| Hooks | `.specify/extensions.yml` — know they exist so a surprise commit mid-unit is explicable. Do not run them yourself; the commands dispatch them |

Getting the separator wrong makes every command you emit wrong. When unsure,
read a prerequisite script's own error text — it prints correctly-separated
command names.

## Probe state, don't guess it

The prerequisite script is the supported machine-readable probe. Its
`--paths-only --json` form never validates and is safe to call at any time; the
other forms *fail* when prerequisites are missing, and that failure is
information — it tells you which step is next.

Two traps worth knowing before they cost you time:

- The reported branch is the **feature directory name**, not
  `git branch --show-current`. A project can sit on `main` with a feature
  active. Never use it as a git branch name.
- `--paths-only` reports paths that **may not exist**. Test for the file; do not
  infer existence from a populated path.

Target a feature for this unit only by setting the per-invocation feature
directory environment variable. Do not rewrite the project's persistent active
feature — that silently changes what the user's next bare command operates on.

## Find the existing feature before creating one

Look for a feature that already covers the focused workstream. Creating a second
feature for work a first one already specifies is the most expensive mistake
available here, because both then drift.

## What each artifact is for

| Artifact | Use |
|---|---|
| A prioritized user story | One roadmap unit — the template requires they be independently testable, which is what a unit is |
| Acceptance scenarios | The unit's acceptance criteria, verbatim where testable |
| Requirement and success ids | Cite them in the ship objective so the report is auditable against the spec |
| Research decisions | Load-bearing. Pin them; do not re-derive a settled decision |
| Data model invariants | Assert them in tests |
| Contracts | What verification actually checks |
| Quickstart scenarios | The unit's proof-of-behaviour script |
| The task list | Real task ids for the ship plan — prefer them over invented ones |
| Constitution | A gate, in both shape and build |

## In build mode

- Seed the ship objective from acceptance scenarios **plus** requirement ids.
- Prefer the task list's own ids in the ship plan, so ship, the task list, and
  the Spec Kit implement command all name the same units.
- Keep the task list synchronized with reality. A unit that ships with its tasks
  unticked leaves the next session unable to tell what is done.
- Run the consistency analysis and the constitution check before completing, and
  record both results honestly — including "not run", when they were not.

## In shape mode

Amendments route to the artifact that owns the fact:

| Finding | Goes to |
|---|---|
| A new or changed requirement | The spec, through the clarification channel |
| A wrong technical approach | The plan, with the deviation recorded |
| A settled open question | The research decisions |
| A unit that is really two | Split the roadmap units; amend the spec if the story split too |

**When implementation contradicts the plan, amend the plan.** A plan that no
longer describes the code is worse than no plan.

## Constitution conflicts

If a principle blocks the unit, stop and surface it. Do not implement around a
`MUST`, and do not dilute the principle to fit. If the principle itself is
wrong, that is an amendment with a version bump — its own unit, and a human's
decision.

## Unresolved markers

A specification still carrying an unresolved-clarification marker in the area
you are about to build is not ready. Run the clarification step, or surface the
question. A unit built on a guess gets rebuilt.
