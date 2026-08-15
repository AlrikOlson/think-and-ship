# Security policy

Report vulnerabilities privately through GitHub:
[Security → Report a vulnerability](https://github.com/AlrikOlson/think-and-ship/security/advisories/new).
Private vulnerability reporting keeps the report between you and the
maintainer until a fix ships; nothing is published when you file it.

Do not open a public issue for a security problem.

## Supported versions

Only the latest released version of `think-and-ship` receives fixes.

## What to expect

This project is maintained by one person. Reports are read and taken
seriously, but there is no guaranteed response time. If a report is valid,
the fix, a release, and credit in the advisory (unless you decline) follow
as fast as one maintainer can make them.

## Scope notes

The MCP server executes shell commands passed to `ship_check` by design —
that is the feature, not a vulnerability. Reports about it should
demonstrate an escalation beyond what the calling agent could already do.
