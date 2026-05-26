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
  think-and-ship init          Set up MCP config for the current project
  think-and-ship init --dry-run  Show what would be written without writing
  think-and-ship init --force    Overwrite existing server config

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

const MCP_SERVERS_CONFIG = {
  deliberate: {
    command: "deliberate-mcp",
    env: {
      DELIBERATE_PERSIST: "true",
      DELIBERATE_ENABLE_SESSIONS: "true",
    },
  },
  resolute: {
    command: "resolute-mcp",
    env: {
      RESOLUTE_PERSIST: "true",
    },
  },
};

const IDE_TARGETS = [
  { name: "Cursor", dir: ".cursor", configFile: ".cursor/mcp.json" },
  { name: "Windsurf", dir: ".windsurf", configFile: ".windsurf/mcp.json" },
  { name: "Claude Code", dir: null, configFile: ".mcp.json" },
];

const PROJECT_TYPES = [
  {
    name: "Rust",
    marker: "Cargo.toml",
    verify: ["cargo test", "cargo clippy --all-targets -- -D warnings"],
  },
  {
    name: "Node",
    marker: "package.json",
    verify: ["npm test", "npm run lint"],
  },
  {
    name: "Python",
    marker: "pyproject.toml",
    verify: ["pytest", "ruff check"],
  },
  {
    name: "Python",
    marker: "setup.py",
    verify: ["pytest", "ruff check"],
  },
  {
    name: "Go",
    marker: "go.mod",
    verify: ["go test ./...", "go vet ./..."],
  },
];

function detectIDE(cwd) {
  for (const target of IDE_TARGETS) {
    if (target.dir && fs.existsSync(path.join(cwd, target.dir))) {
      return target;
    }
  }
  return IDE_TARGETS[IDE_TARGETS.length - 1];
}

function detectProject(cwd) {
  for (const pt of PROJECT_TYPES) {
    if (fs.existsSync(path.join(cwd, pt.marker))) {
      return pt;
    }
  }
  return null;
}

function readJsonSafe(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, "utf8"));
  } catch {
    return null;
  }
}

function cmdInit() {
  const args = process.argv.slice(3);
  const dryRun = args.includes("--dry-run");
  const force = args.includes("--force");
  const cwd = process.cwd();

  console.log(`think-and-ship init v${VERSION}\n`);

  const ide = detectIDE(cwd);
  const project = detectProject(cwd);
  const configPath = path.join(cwd, ide.configFile);

  console.log(`  IDE:     ${ide.name} (${ide.configFile})`);
  console.log(`  Project: ${project ? `${project.name} (${project.marker})` : "unknown"}`);
  if (project) {
    console.log(`  Verify:  ${project.verify.join(", ")}`);
  }
  console.log();

  const existing = readJsonSafe(configPath);
  const alreadyHasDeliberate = existing?.mcpServers?.deliberate;
  const alreadyHasResolute = existing?.mcpServers?.resolute;

  if (alreadyHasDeliberate && alreadyHasResolute && !force) {
    console.log(
      `Both servers already configured in ${ide.configFile}\n` +
        "Nothing to do. Use --force to overwrite."
    );
    return;
  }

  let config;
  if (existing && !force) {
    config = { ...existing };
    if (!config.mcpServers) config.mcpServers = {};
    if (!alreadyHasDeliberate) {
      config.mcpServers.deliberate = MCP_SERVERS_CONFIG.deliberate;
    }
    if (!alreadyHasResolute) {
      config.mcpServers.resolute = MCP_SERVERS_CONFIG.resolute;
    }
  } else {
    config = {
      mcpServers: {
        ...(existing && !force ? existing.mcpServers : {}),
        ...MCP_SERVERS_CONFIG,
      },
    };
    if (existing && force) {
      config = { ...existing, mcpServers: { ...existing.mcpServers, ...MCP_SERVERS_CONFIG } };
    }
  }

  const output = JSON.stringify(config, null, 2) + "\n";

  if (dryRun) {
    console.log(`Would write to ${ide.configFile}:\n`);
    console.log(output);
    return;
  }

  const dir = path.dirname(configPath);
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }
  fs.writeFileSync(configPath, output);

  const added = [];
  if (!alreadyHasDeliberate || force) added.push("deliberate");
  if (!alreadyHasResolute || force) added.push("resolute");

  console.log(`Wrote ${ide.configFile}`);
  console.log(`  Added: ${added.join(", ")}`);
  if (existing && !force) {
    console.log("  Preserved existing servers");
  }

  console.log("\nYou're ready! Start a conversation and both servers will connect.");
  if (project) {
    console.log(`\nDetected ${project.name} project — your agent can use:`);
    for (const cmd of project.verify) {
      console.log(`  ${cmd}`);
    }
  }
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
