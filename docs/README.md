# Documentation

Every document here is one of four kinds, following
[Diátaxis](https://diataxis.fr/): a **tutorial** teaches by walking,
a **how-to** solves one task, **reference** states facts to look up, and an
**explanation** gives the reasoning behind the design. Each document names
its kind in its first line. Start with the tutorial if you are new; go
straight to reference if you are not.

## Tutorial

| Document | What it teaches |
|---|---|
| [TUTORIAL.md](TUTORIAL.md) | A first session, end to end — install, let an agent record real work, then look at the trace it produced |

## How-to guides

| Document | The task |
|---|---|
| [OBSERVABILITY.md](OBSERVABILITY.md) | Export the trace as OpenTelemetry, emit live spans, join the caller's trace |
| [SHARED_TRACES.md](SHARED_TRACES.md) | Mirror traces into your repo as git-native Agent Trace records |
| [DEPLOYMENT.md](DEPLOYMENT.md) | Run the server remotely over Streamable HTTP, with auth |
| [RELEASING.md](RELEASING.md) | Cut a release (maintainers) |
| [MIGRATION.md](MIGRATION.md) | Moving an existing skills install to the current destination and profile |

The [deploy/](deploy/) directory holds the Dockerfile and release tooling
these guides reference.

## Reference

| Document | What it describes |
|---|---|
| [TOOLS.md](TOOLS.md) | All 44 MCP tools, cross-references, persistence, environment variables |
| [CLI.md](CLI.md) | Every CLI command and the config `init` writes |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Crate layout and the design contract |
| [SCHEMA.md](SCHEMA.md) | The on-disk and shared-trace wire formats |
| [UNIFIED_CONTRACT.md](UNIFIED_CONTRACT.md) | The cloud record envelope — the backend-agnostic wire form of any record |
| [SIGNAL_CONTRACT.md](SIGNAL_CONTRACT.md) | The signal submission contract for the cloud service |
| [STYLE.md](STYLE.md) | The documentation voice, as citable rules (contributors) |

## Explanation

| Document | The question it answers |
|---|---|
| [WHY_RECORD_REASONING.md](WHY_RECORD_REASONING.md) | Why record an agent's reasoning at all? |
| [WORKFLOWS.md](WORKFLOWS.md) | How do the tool families combine into development loops? |

## Elsewhere in the repository

[README.md](../README.md) is the front door;
[CONTRIBUTING.md](../CONTRIBUTING.md) has the build, gates, and hooks;
[SECURITY.md](../SECURITY.md) has vulnerability reporting;
[CHANGELOG.md](../CHANGELOG.md) has release history.
