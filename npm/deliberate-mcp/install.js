#!/usr/bin/env node
// Deprecation stub. The real `deliberate-mcp` server has merged into the
// unified `think-and-ship` binary as of v0.3.2. This package no longer
// downloads or builds anything during postinstall — it only prints a
// migration pointer. Exit 0 so the install itself does not fail.

const lines = [
  "",
  "deliberate-mcp is deprecated and has merged into `think-and-ship`.",
  "",
  "  npm uninstall -g deliberate-mcp",
  "  npm install -g think-and-ship",
  "",
  "Update your MCP config to call `think-and-ship serve` and use the",
  "`think_*` tool names. The old `deliberate_*` names remain wired as",
  "deprecated aliases through v0.2.x.",
  "",
  "See https://github.com/AlrikOlson/think-and-ship for details.",
  "",
];
process.stderr.write(lines.join("\n"));
process.exit(0);
