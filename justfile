# justfile — build + install think-and-ship globally, cross-platform.
#
# Prerequisites:
#   - just            https://github.com/casey/just  (`cargo install just` / `brew install just` / `winget install Casey.Just`)
#   - a Rust toolchain (rustup). On Windows, `cargo install` builds from source
#     and needs the MSVC C++ build tools (or a GNU toolchain).
#   - the `claude` CLI on PATH for the `register` / `global` recipes (optional —
#     `install` alone just puts the binary on PATH).
#
# `global` and `fresh` write into your HOME: the binary into the cargo bin dir,
# the MCP registration into Claude Code's user config, and the core agent skills
# into each detected agent's skills directory. `install` alone touches only the
# binary.
#
# Quick start (build + register + skills, for every project):
#   just global
#
# Each recipe is a single program invocation so it runs unchanged under sh
# (macOS/Linux) and PowerShell (Windows). On Windows we force PowerShell so a
# missing `sh` (no Git Bash) doesn't break things.

set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# The crate that produces the `think-and-ship` binary.
crate := "crates/think-and-ship"
# MCP server name + the env that turns on cross-session on-disk persistence.
server := "think-and-ship"

# Show the recipe list when run with no arguments.
_default:
    @just --list

# `skills` runs AFTER `install` on purpose: it invokes the binary that `install`
# just put on PATH, so the skills written are the ones this checkout carries
# rather than whatever an older install had embedded.
#
# Build + register globally (user scope, persistence on) + install the core skills.
global: install register skills
    @echo "think-and-ship installed, registered at user scope, and skills written. Restart your MCP client (e.g. /mcp reconnect) to load the tools."

# Use after pulling changes when you want zero chance of a stale binary or
# registration. `clean -p` keeps dependency artifacts so it isn't punishingly slow.
#
# Everything from scratch: clean, rebuild, install, register, skills, verify.
fresh: clean install register skills doctor
    @echo "Fresh install complete. Restart your MCP client (e.g. /mcp reconnect) to load the new binary."

# Remove this crate's build artifacts so the next build is from source.
clean:
    cargo clean -p {{server}}

# Compile and install the binary into the cargo bin dir (~/.cargo/bin or
# %USERPROFILE%\.cargo\bin), which rustup already put on PATH. --force lets it
# overwrite an older install; --locked builds against the committed Cargo.lock.
install:
    cargo install --path {{crate}} --locked --force

# Register think-and-ship with Claude Code for ALL projects (user scope), with
# persistence enabled. The leading `-` on the remove makes just ignore its exit
# code, so re-running `register` is idempotent (remove-then-add).
register:
    -claude mcp remove {{server}} --scope user
    claude mcp add {{server}} --scope user -e THINK_AND_SHIP_PERSIST=true -- {{server}} serve

# Additive and conservative: a skill directory that already exists with local
# edits is REPORTED and left alone rather than overwritten, so re-running this
# cannot destroy a customization. Legacy skills are not installed — pass
# `--profile legacy` yourself if you still want them.
#
# Install the core skills (switch-work, advance-work) for every detected agent.
skills:
    {{server}} skills install

# Preview what `skills` would write, without touching a single file.
skills-preview:
    {{server}} skills install --dry-run

# Retires the old ~/.codex/skills, which Codex does not read and which another
# agent's compatibility list still discovers — so a stale copy there can answer
# instead of the current skill. Add `--apply` to actually remove, and even then
# only copies it can prove are unmodified are touched.
#
# Preview retiring skill directories this installer no longer writes.
skills-migrate:
    {{server}} skills migrate

# Remove the user-scope MCP registration (leaves the binary installed).
unregister:
    claude mcp remove {{server}} --scope user

# Full teardown: unregister, then uninstall the binary.
uninstall:
    -claude mcp remove {{server}} --scope user
    cargo uninstall {{server}}

# Rebuild + reinstall, keeping the existing registration.
reinstall: install
    @echo "Reinstalled {{server}}. Restart your MCP client to pick up the new binary."

# Diagnose the install: binary on PATH, config, data dir, CLAUDE.md.
doctor:
    {{server}} doctor

# Confirm the server is wired up in Claude Code.
status:
    claude mcp list

# Point git at the tracked hooks (commit-msg rejects process narration in
# commit messages — see docs/STYLE.md S8).
hooks:
    git config core.hooksPath .githooks

# Project verification (matches CLAUDE.md): tests + clippy across the workspace.
verify: test lint

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Store the WorkOS API key as a LOCAL dev secret in backend/.dev.vars (gitignored,
# loaded by `wrangler dev`). Prompts silently — the value is never echoed, logged,
# or committed. Idempotent: replaces any existing WORKOS_API_KEY line.
# (For a production deploy, use `npx wrangler secret put WORKOS_API_KEY` instead.)
workos-secret:
    #!/usr/bin/env bash
    set -euo pipefail
    cd backend
    read -rs -p "Paste WORKOS_API_KEY (sk_...): " K
    echo
    touch .dev.vars
    grep -v '^WORKOS_API_KEY=' .dev.vars > .dev.vars.tmp 2>/dev/null || true
    mv .dev.vars.tmp .dev.vars 2>/dev/null || true
    printf 'WORKOS_API_KEY=%s\n' "$K" >> .dev.vars
    echo "✓ wrote WORKOS_API_KEY to backend/.dev.vars (gitignored)"
