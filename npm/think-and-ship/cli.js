#!/usr/bin/env node

const { execSync } = require("child_process");
const path = require("path");
const fs = require("fs");

const VERSION = require("./package.json").version;

const SERVERS = [
  {
    name: "deliberate-mcp",
    npm: "deliberate-mcp",
    description: "structured reasoning traces",
  },
  {
    name: "resolute-mcp",
    npm: "resolute-mcp",
    description: "structured execution tracking",
  },
];

function isRealBinary(filePath) {
  try {
    const buf = fs.readFileSync(filePath);
    if (buf.length < 4) return false;
    const head = buf.toString("utf8", 0, Math.min(buf.length, 256));
    if (head.includes("binary not installed")) return false;
    return true;
  } catch {
    return false;
  }
}

function findBinary(name) {
  const candidates = [
    path.join(__dirname, "node_modules", name, "bin", name),
    path.join(__dirname, "..", name, "bin", name),
  ];

  for (const p of candidates) {
    if (fs.existsSync(p) && isRealBinary(p)) return p;
  }

  try {
    const which = execSync(`which ${name} 2>/dev/null`, {
      encoding: "utf8",
    }).trim();
    if (which) return which;
  } catch {}

  return null;
}

function getServerVersion(binPath) {
  try {
    const out = execSync(`"${binPath}" --version 2>&1`, {
      encoding: "utf8",
      timeout: 5000,
    });
    const firstLine = out.trim().split("\n")[0];
    return firstLine || "installed (version unknown)";
  } catch (e) {
    if (e.stdout || e.stderr) {
      const out = (e.stdout || e.stderr || "").trim();
      const firstLine = out.split("\n")[0];
      if (firstLine && !firstLine.includes("not installed")) return firstLine;
    }
    return null;
  }
}

function cmdVersion() {
  console.log(`think-and-ship v${VERSION}\n`);
  for (const server of SERVERS) {
    const bin = findBinary(server.name);
    if (!bin) {
      console.log(`  ${server.name}: not found`);
      continue;
    }
    const ver = getServerVersion(bin);
    console.log(`  ${server.name}: ${ver || "binary found but --version failed"}`);
  }
}

function cmdCheck() {
  console.log(`think-and-ship v${VERSION} — checking servers...\n`);
  let allOk = true;

  for (const server of SERVERS) {
    const bin = findBinary(server.name);
    if (!bin) {
      console.log(`  [FAIL] ${server.name}: binary not found`);
      allOk = false;
      continue;
    }

    const ver = getServerVersion(bin);
    if (!ver) {
      console.log(`  [FAIL] ${server.name}: binary at ${bin} but --version failed`);
      allOk = false;
      continue;
    }

    console.log(`  [ OK ] ${server.name}: ${ver}`);
  }

  console.log();
  if (allOk) {
    console.log("All servers operational. Ready to use with any MCP client.");
  } else {
    console.log(
      "Some servers missing. Try reinstalling:\n  npm install -g think-and-ship"
    );
    process.exit(1);
  }
}

function cmdHelp() {
  console.log(`think-and-ship v${VERSION}

Two MCP servers. One thinks, one ships.

Usage:
  think-and-ship --check       Verify both servers are installed and working
  think-and-ship --version     Show version info for all components
  think-and-ship --help        Show this help message
  think-and-ship init          Set up MCP config for the current project (coming soon)

Install:
  npm install -g think-and-ship    Install globally
  npx think-and-ship --check       Run without installing

Servers:
  deliberate-mcp    Structured reasoning traces (11 tools)
  resolute-mcp      Structured execution tracking (11 tools)

Configure (Claude Code):
  Add to your project's .mcp.json:

  {
    "mcpServers": {
      "deliberate": {
        "command": "deliberate-mcp",
        "env": {
          "DELIBERATE_PERSIST": "true",
          "DELIBERATE_ENABLE_SESSIONS": "true"
        }
      },
      "resolute": {
        "command": "resolute-mcp",
        "env": { "RESOLUTE_PERSIST": "true" }
      }
    }
  }

More info: https://github.com/AlrikOlson/think-and-ship`);
}

function cmdInit() {
  console.log(
    "think-and-ship init is coming in Phase 8.\n\n" +
      "For now, manually add the servers to your .mcp.json:\n\n" +
      '  think-and-ship --help    (see "Configure" section)'
  );
}

const arg = process.argv[2];

switch (arg) {
  case "--version":
  case "-v":
    cmdVersion();
    break;
  case "--check":
  case "check":
    cmdCheck();
    break;
  case "--help":
  case "-h":
  case undefined:
    cmdHelp();
    break;
  case "init":
    cmdInit();
    break;
  default:
    console.error(`Unknown command: ${arg}\n`);
    cmdHelp();
    process.exit(1);
}
