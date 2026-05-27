#!/usr/bin/env node

const { execSync, spawn } = require("child_process");
const path = require("path");
const fs = require("fs");
const os = require("os");

const VERSION = require("./package.json").version;
const BIN_NAME = "think-and-ship";

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

function findBinary() {
  const candidates = [
    path.join(__dirname, "bin", BIN_NAME),
    path.join(__dirname, "..", BIN_NAME, "bin", BIN_NAME),
  ];
  for (const p of candidates) {
    if (fs.existsSync(p) && isRealBinary(p)) return p;
  }
  try {
    const which = execSync(`which ${BIN_NAME} 2>/dev/null`, {
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
    return out.trim().split("\n")[0] || "installed (version unknown)";
  } catch (e) {
    const out = (e.stdout || e.stderr || "").toString().trim();
    const firstLine = out.split("\n")[0];
    if (firstLine && !firstLine.includes("not installed")) return firstLine;
    return null;
  }
}

function cmdVersion() {
  console.log(`think-and-ship wrapper v${VERSION}\n`);
  const bin = findBinary();
  if (!bin) {
    console.log("  binary: not found");
    return;
  }
  const ver = getServerVersion(bin);
  console.log(`  binary: ${ver || "found but --version failed"}`);
  console.log(`  path:   ${bin}`);
}

function cmdCheck() {
  console.log(`think-and-ship v${VERSION} — checking install...\n`);
  const bin = findBinary();
  if (!bin) {
    console.log("  [FAIL] think-and-ship binary not found");
    console.log("         Fix: npm install -g think-and-ship");
    process.exit(1);
  }
  const ver = getServerVersion(bin);
  if (!ver) {
    console.log(`  [FAIL] binary at ${bin} but --version failed`);
    process.exit(1);
  }
  console.log(`  [ OK ] ${ver}`);
  console.log(`  [ OK ] ${bin}`);
  console.log("\nServer operational. Configure your MCP client with:");
  console.log('         { "command": "think-and-ship", "args": ["serve"] }');
}

function cmdHelp() {
  console.log(`think-and-ship v${VERSION}

One MCP server. Two tool families: think_* (reasoning) + ship_* (execution).

Usage:
  think-and-ship init                   Set up MCP config for the current project
  think-and-ship init --full            MCP config + CLAUDE.md in one shot
  think-and-ship init --with-claude-md  Also generate CLAUDE.md with tool reference
  think-and-ship init --dry-run         Show what would be written without writing
  think-and-ship init --force           Overwrite existing config
  think-and-ship doctor                 Diagnose setup issues
  think-and-ship status                 Show project info and config state
  think-and-ship --check                Verify the binary is installed
  think-and-ship --version              Show wrapper + server version
  think-and-ship --help                 Show this help message

Note:
  This wrapper is the install/init/doctor helper. The actual MCP server
  runs from the underlying Rust binary via \`think-and-ship serve\`,
  which is what MCP clients invoke.

Install:
  npm install -g think-and-ship         Install globally
  npx think-and-ship --check            Run without installing

Tool families:
  think_*    Reasoning trace: think_record_step, think_pin_step, ...   (11 tools)
  ship_*     Execution trace: ship_set_objective, ship_record, ...     (11 tools)

  The old deliberate_* and resolute_* names remain wired as deprecated
  aliases through v0.2.x with _meta.deprecation_warning set.

Configure (Claude Code):
  Add to your project's .mcp.json:

  {
    "mcpServers": {
      "think-and-ship": {
        "command": "think-and-ship",
        "args": ["serve"],
        "env": {
          "THINK_AND_SHIP_PERSIST": "true"
        }
      }
    }
  }

More info: https://github.com/AlrikOlson/think-and-ship`);
}

const MCP_SERVER_NAME = "think-and-ship";
const MCP_SERVER_CONFIG = {
  command: "think-and-ship",
  args: ["serve"],
  env: {
    THINK_AND_SHIP_PERSIST: "true",
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
  { name: "Node", marker: "package.json", verify: ["npm test", "npm run lint"] },
  { name: "Python", marker: "pyproject.toml", verify: ["pytest", "ruff check"] },
  { name: "Python", marker: "setup.py", verify: ["pytest", "ruff check"] },
  { name: "Go", marker: "go.mod", verify: ["go test ./...", "go vet ./..."] },
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
    if (fs.existsSync(path.join(cwd, pt.marker))) return pt;
  }
  return null;
}

const CLAUDE_MD_MARKER = "<!-- think-and-ship -->";

function generateClaudeMd(project) {
  const verifyBlock = project
    ? `\n## Verification\n\nThis is a ${project.name} project. Use these commands to verify changes:\n\n${project.verify.map((c) => `- \`${c}\``).join("\n")}\n`
    : "";

  return `${CLAUDE_MD_MARKER}
# think-and-ship

One MCP server is configured: **think-and-ship**, exposing two tool families.

## Tool families

| Family | Purpose | Key tools |
|--------|---------|-----------|
| **think_*** | Reasoning trace: record steps, branch hypotheses, pin conclusions | \`think_record_step\`, \`think_pin_step\`, \`think_trace_checkpoint\` |
| **ship_*** | Execution trace: objectives, tasks, actions, quality gates | \`ship_set_objective\`, \`ship_plan\`, \`ship_start\`, \`ship_record\`, \`ship_check\`, \`ship_ship\` |

The old \`deliberate_*\` and \`resolute_*\` names remain wired as deprecated
aliases through v0.2.x (the server emits \`_meta.deprecation_warning\` on
each). Prefer the canonical \`think_*\` / \`ship_*\` names in new prompts.

## Cross-referencing

Link reasoning to execution:
- On \`think_record_step\`, pass \`execution_ref: "task:<id>"\` to point at a ship_* task.
- On \`ship_record\`, pass \`deliberate_step: <N>\` to point back at the motivating think_* step.

Both halves resolve the same project identity from the working directory so
traces auto-correlate.

## Quick-start workflow

1. \`ship_set_objective\` — define the goal
2. \`ship_plan\` — break into tasks
3. \`think_record_step\` — record your reasoning (open)
4. \`ship_start\` → \`ship_record\` → \`ship_complete\` — do the work
5. \`ship_check\` — record test/lint results
6. \`ship_ship\` — finalize the objective
7. \`think_record_step\` — record outcome (close)
${verifyBlock}`;
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
  const withClaudeMd =
    args.includes("--with-claude-md") || args.includes("--full");
  const cwd = process.cwd();

  console.log(`think-and-ship init v${VERSION}\n`);

  const ide = detectIDE(cwd);
  const project = detectProject(cwd);
  const configPath = path.join(cwd, ide.configFile);

  console.log(`  IDE:     ${ide.name} (${ide.configFile})`);
  console.log(
    `  Project: ${project ? `${project.name} (${project.marker})` : "unknown"}`
  );
  if (project) {
    console.log(`  Verify:  ${project.verify.join(", ")}`);
  }
  console.log();

  const existing = readJsonSafe(configPath);
  const alreadyConfigured = existing?.mcpServers?.[MCP_SERVER_NAME];

  if (alreadyConfigured && !force) {
    console.log(`Already configured in ${ide.configFile}`);
  } else {
    let config;
    if (existing) {
      config = { ...existing };
      if (!config.mcpServers) config.mcpServers = {};
      config.mcpServers[MCP_SERVER_NAME] = MCP_SERVER_CONFIG;
    } else {
      config = { mcpServers: { [MCP_SERVER_NAME]: MCP_SERVER_CONFIG } };
    }

    const output = JSON.stringify(config, null, 2) + "\n";

    if (dryRun) {
      console.log(`Would write to ${ide.configFile}:\n`);
      console.log(output);
    } else {
      const dir = path.dirname(configPath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      fs.writeFileSync(configPath, output);
      console.log(`Wrote ${ide.configFile}`);
      console.log(`  Added: ${MCP_SERVER_NAME}`);
      if (existing) {
        console.log("  Preserved existing servers");
      }
    }
  }

  if (!withClaudeMd) {
    console.log("\nYou're ready! Start a conversation and the server will connect.");
    if (project) {
      console.log(`\nDetected ${project.name} project — your agent can use:`);
      for (const cmd of project.verify) {
        console.log(`  ${cmd}`);
      }
    }
    console.log(
      "\nTip: run with --with-claude-md to also generate a CLAUDE.md tool reference."
    );
    return;
  }

  const claudeMdPath = path.join(cwd, "CLAUDE.md");
  const claudeMdContent = generateClaudeMd(project);
  const existingClaudeMd = fs.existsSync(claudeMdPath)
    ? fs.readFileSync(claudeMdPath, "utf8")
    : null;

  if (existingClaudeMd && existingClaudeMd.includes(CLAUDE_MD_MARKER) && !force) {
    console.log(
      "CLAUDE.md already contains think-and-ship section. Use --force to overwrite."
    );
  } else if (dryRun) {
    if (existingClaudeMd) {
      console.log("Would append to CLAUDE.md:\n");
    } else {
      console.log("Would create CLAUDE.md:\n");
    }
    console.log(claudeMdContent);
  } else if (existingClaudeMd && existingClaudeMd.includes(CLAUDE_MD_MARKER) && force) {
    const before = existingClaudeMd.split(CLAUDE_MD_MARKER)[0];
    fs.writeFileSync(
      claudeMdPath,
      before.trimEnd() + "\n\n" + claudeMdContent + "\n"
    );
    console.log("Replaced think-and-ship section in CLAUDE.md");
  } else if (existingClaudeMd) {
    fs.writeFileSync(
      claudeMdPath,
      existingClaudeMd.trimEnd() + "\n\n" + claudeMdContent + "\n"
    );
    console.log("Appended think-and-ship section to CLAUDE.md");
  } else {
    fs.writeFileSync(claudeMdPath, claudeMdContent + "\n");
    console.log("Created CLAUDE.md with think-and-ship tool reference");
  }

  console.log("\nYou're ready! The agent will see the tool reference on first prompt.");
}

function cmdDoctor() {
  console.log(`think-and-ship doctor v${VERSION}\n`);
  const cwd = process.cwd();
  let issues = 0;

  const bin = findBinary();
  if (!bin) {
    console.log("  [FAIL] think-and-ship binary not found");
    console.log("         Fix: npm install -g think-and-ship");
    issues++;
  } else {
    const ver = getServerVersion(bin);
    if (ver) {
      console.log(`  [ OK ] binary: ${ver} (${bin})`);
    } else {
      console.log(`  [WARN] found at ${bin} but --version failed`);
      issues++;
    }
  }

  console.log();

  const ide = detectIDE(cwd);
  const configPath = path.join(cwd, ide.configFile);
  if (fs.existsSync(configPath)) {
    const config = readJsonSafe(configPath);
    if (!config) {
      console.log(`  [FAIL] ${ide.configFile}: exists but invalid JSON`);
      console.log("         Fix: check syntax or run: think-and-ship init --force");
      issues++;
    } else if (!config.mcpServers?.[MCP_SERVER_NAME]) {
      console.log(`  [WARN] ${ide.configFile}: missing think-and-ship server entry`);
      console.log("         Fix: think-and-ship init");
      issues++;
    } else {
      console.log(`  [ OK ] ${ide.configFile}: configured`);
    }
  } else {
    console.log(`  [WARN] ${ide.configFile}: not found`);
    console.log("         Fix: think-and-ship init");
    issues++;
  }

  const dataRoot = path.join(os.homedir(), ".local", "share", "think-and-ship");
  const partitions = [
    { name: "think", dir: path.join(dataRoot, "think", "sessions") },
    { name: "ship", dir: path.join(dataRoot, "ship", "sessions") },
  ];
  for (const { name, dir } of partitions) {
    if (fs.existsSync(dir)) {
      try {
        fs.accessSync(dir, fs.constants.W_OK);
        console.log(`  [ OK ] ${name} sessions: ${dir}`);
      } catch {
        console.log(`  [FAIL] ${name} sessions: ${dir} (not writable)`);
        console.log(`         Fix: chmod u+w "${dir}"`);
        issues++;
      }
    } else {
      console.log(`  [ -- ] ${name} sessions: ${dir} (will be created on first use)`);
    }
  }

  // v0.1.x legacy dirs — informational, not a failure
  const legacyDirs = [
    { name: "deliberate-mcp (v0.1.x)", dir: path.join(os.homedir(), ".local", "share", "deliberate-mcp") },
    { name: "resolute-mcp (v0.1.x)", dir: path.join(os.homedir(), ".local", "share", "resolute-mcp") },
  ];
  for (const { name, dir } of legacyDirs) {
    if (fs.existsSync(dir)) {
      console.log(
        `  [INFO] ${name} dir still present at ${dir}; the server will auto-migrate on first run.`
      );
    }
  }

  const claudeMdPath = path.join(cwd, "CLAUDE.md");
  if (fs.existsSync(claudeMdPath)) {
    const content = fs.readFileSync(claudeMdPath, "utf8");
    if (content.includes(CLAUDE_MD_MARKER)) {
      console.log("  [ OK ] CLAUDE.md: think-and-ship section present");
    } else {
      console.log("  [ -- ] CLAUDE.md: exists but no think-and-ship section");
      console.log("         Tip: think-and-ship init --with-claude-md");
    }
  } else {
    console.log("  [ -- ] CLAUDE.md: not found");
    console.log("         Tip: think-and-ship init --full");
  }

  console.log();
  if (issues === 0) {
    console.log("No issues found. Everything looks good.");
  } else {
    console.log(`Found ${issues} issue${issues > 1 ? "s" : ""}. See Fix suggestions above.`);
  }
}

function cmdStatus() {
  console.log(`think-and-ship v${VERSION}\n`);
  const cwd = process.cwd();

  const ide = detectIDE(cwd);
  const project = detectProject(cwd);

  console.log(`  Project:  ${path.basename(cwd)}`);
  console.log(`  Dir:      ${cwd}`);
  console.log(`  IDE:      ${ide.name} (${ide.configFile})`);
  console.log(
    `  Type:     ${project ? `${project.name} (${project.marker})` : "unknown"}`
  );
  if (project) {
    console.log(`  Verify:   ${project.verify.join(", ")}`);
  }

  console.log();

  const configPath = path.join(cwd, ide.configFile);
  if (fs.existsSync(configPath)) {
    const config = readJsonSafe(configPath);
    if (config?.mcpServers) {
      const servers = Object.keys(config.mcpServers);
      console.log(`  MCP servers: ${servers.join(", ")}`);
    }
  } else {
    console.log("  MCP config: not found (run: think-and-ship init)");
  }

  const claudeMdPath = path.join(cwd, "CLAUDE.md");
  if (fs.existsSync(claudeMdPath)) {
    const content = fs.readFileSync(claudeMdPath, "utf8");
    console.log(
      `  CLAUDE.md: ${
        content.includes(CLAUDE_MD_MARKER) ? "has tool reference" : "exists (no tool reference)"
      }`
    );
  } else {
    console.log("  CLAUDE.md: not found");
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
  case "doctor":
    cmdDoctor();
    break;
  case "status":
    cmdStatus();
    break;
  default:
    // Forward unknown args to the real binary (so `npx think-and-ship serve` works).
    {
      const bin = findBinary();
      if (!bin) {
        console.error(`Unknown command: ${arg}\n`);
        cmdHelp();
        process.exit(1);
      }
      const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
      child.on("exit", (code) => process.exit(code ?? 1));
    }
}
