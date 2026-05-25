#!/usr/bin/env node

const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const os = require("os");
const https = require("https");

const VERSION = require("./package.json").version;
const REPO = "AlrikOlson/think-and-ship";
const BIN_NAME = "resolute-mcp";
const BIN_DIR = path.join(__dirname, "bin");

function getPlatformKey() {
  const platform = os.platform();
  const arch = os.arch();

  if (platform === "darwin" && arch === "arm64") return "aarch64-apple-darwin";
  if (platform === "darwin" && arch === "x64") return "x86_64-apple-darwin";
  if (platform === "linux" && arch === "x64") return "x86_64-unknown-linux-gnu";
  if (platform === "linux" && arch === "arm64")
    return "aarch64-unknown-linux-gnu";

  return null;
}

function tryCargoInstall() {
  try {
    execSync("cargo --version", { stdio: "ignore" });
  } catch {
    return false;
  }

  console.log(`${BIN_NAME}: building from source with cargo...`);
  try {
    execSync(
      `cargo install --root "${path.join(__dirname, ".cargo-install")}" --path "${path.resolve(__dirname, "..", "..", "crates", BIN_NAME)}" 2>&1`,
      { stdio: "inherit" }
    );
    const built = path.join(
      __dirname,
      ".cargo-install",
      "bin",
      BIN_NAME
    );
    if (fs.existsSync(built)) {
      fs.mkdirSync(BIN_DIR, { recursive: true });
      fs.copyFileSync(built, path.join(BIN_DIR, BIN_NAME));
      fs.chmodSync(path.join(BIN_DIR, BIN_NAME), 0o755);
      return true;
    }
  } catch (e) {
    console.error(`${BIN_NAME}: cargo install failed: ${e.message}`);
  }
  return false;
}

function download(url) {
  return new Promise((resolve, reject) => {
    https
      .get(url, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          return download(res.headers.location).then(resolve, reject);
        }
        if (res.statusCode !== 200) {
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}

async function tryGithubRelease() {
  const platformKey = getPlatformKey();
  if (!platformKey) return false;

  const assetName = `${BIN_NAME}-v${VERSION}-${platformKey}.tar.gz`;
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${assetName}`;

  console.log(`${BIN_NAME}: downloading prebuilt binary...`);
  try {
    const tarball = await download(url);
    fs.mkdirSync(BIN_DIR, { recursive: true });
    const tmpTar = path.join(os.tmpdir(), assetName);
    fs.writeFileSync(tmpTar, tarball);
    execSync(`tar xzf "${tmpTar}" -C "${BIN_DIR}"`, { stdio: "ignore" });
    fs.unlinkSync(tmpTar);
    const binPath = path.join(BIN_DIR, BIN_NAME);
    if (fs.existsSync(binPath)) {
      fs.chmodSync(binPath, 0o755);
      return true;
    }
  } catch (e) {
    console.error(`${BIN_NAME}: download failed: ${e.message}`);
  }
  return false;
}

async function main() {
  const binPath = path.join(BIN_DIR, BIN_NAME);
  if (fs.existsSync(binPath)) {
    console.log(`${BIN_NAME}: binary already exists, skipping install`);
    return;
  }

  if (await tryGithubRelease()) {
    console.log(`${BIN_NAME}: installed prebuilt binary`);
    return;
  }

  if (tryCargoInstall()) {
    console.log(`${BIN_NAME}: built from source`);
    return;
  }

  console.error(
    `${BIN_NAME}: could not install. Either:\n` +
      `  1. Publish a GitHub release with prebuilt binaries, or\n` +
      `  2. Install Rust (https://rustup.rs) and run: cargo install --path crates/${BIN_NAME}`
  );
  process.exit(1);
}

main();
