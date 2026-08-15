#!/usr/bin/env bash
# Idempotently configure the RELEASE_PLZ_TOKEN repo secret used by
# .github/workflows/release-plz.yml.
#
# Why this exists: release-plz pushes the `v<version>` tag. GitHub will NOT
# fire downstream workflows (release.yml: binaries + npm) for a tag created
# with the default GITHUB_TOKEN — so release-plz must authenticate with a PAT
# (or GitHub App token). This script wires that token in.
#
# Idempotent: re-running is a no-op once the secret exists (unless --force).
# GitHub has no API to *create* a PAT, so the token value comes from you
# (a 30-second web step) or an env var; everything else is automated.
#
# Runs on macOS, Linux, and Windows (Git Bash / MSYS2). On Windows invoke it
# through bash — `bash docs/deploy/setup-release-plz-token.sh` — since
# PowerShell will not execute a shell script directly.
#
# Usage:
#   bash docs/deploy/setup-release-plz-token.sh                 # interactive: opens the PAT page, paste it
#   RELEASE_PLZ_TOKEN=ghp_xxx bash docs/deploy/setup-release-plz-token.sh   # non-interactive (CI)
#   bash docs/deploy/setup-release-plz-token.sh --from-gh-token # reuse your gh CLI token (see warning)
#   bash docs/deploy/setup-release-plz-token.sh --force         # replace an existing secret
set -euo pipefail

SECRET="RELEASE_PLZ_TOKEN"
FORCE=0
FROM_GH=0
for arg in "$@"; do
  case "$arg" in
    --force) FORCE=1 ;;
    --from-gh-token) FROM_GH=1 ;;
    -h | --help)
      sed -n '2,20p' "$0"
      exit 0
      ;;
    *)
      echo "unknown arg: $arg (try --help)" >&2
      exit 2
      ;;
  esac
done

command -v gh >/dev/null 2>&1 || {
  echo "gh CLI not found — install from https://cli.github.com" >&2
  exit 1
}
gh auth status >/dev/null 2>&1 || {
  echo "gh is not authenticated — run: gh auth login" >&2
  exit 1
}

# Resolve the repo (slug) from the current checkout, falling back to the canonical one.
REPO="${THINK_AND_SHIP_REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo AlrikOlson/think-and-ship)}"

# Ask the API directly rather than grepping `gh secret list`: the list output
# is a human table whose columns and line endings vary by gh version and
# platform (CRLF under Git Bash), and a prefix grep would also match a secret
# merely NAMED like ours. 200 = exists, 404 = does not.
secret_exists() {
  gh api "repos/${REPO}/actions/secrets/${SECRET}" >/dev/null 2>&1
}

# Open a URL in the user's browser, or say nothing and let the printed URL
# stand. macOS has `open`, Linux desktops `xdg-open`, WSL `wslview`; Windows
# has none of them, which is why the browser step used to be a silent no-op
# under Git Bash. `explorer.exe` exits non-zero even when it succeeds, so
# every branch is `|| true` and the URL is printed either way.
open_url() {
  _url="$1"
  if command -v open >/dev/null 2>&1; then
    open "$_url" >/dev/null 2>&1 || true
  elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$_url" >/dev/null 2>&1 || true
  elif command -v wslview >/dev/null 2>&1; then
    wslview "$_url" >/dev/null 2>&1 || true
  elif command -v powershell.exe >/dev/null 2>&1; then
    powershell.exe -NoProfile -Command "Start-Process '$_url'" >/dev/null 2>&1 || true
  elif command -v explorer.exe >/dev/null 2>&1; then
    explorer.exe "$_url" >/dev/null 2>&1 || true
  fi
}

# ── Idempotency ──────────────────────────────────────────────────────────────
if [ "$FORCE" -eq 0 ] && secret_exists; then
  echo "✓ ${SECRET} already set on ${REPO} — nothing to do (use --force to replace)."
  exit 0
fi

# ── Resolve the token value ──────────────────────────────────────────────────
token="${RELEASE_PLZ_TOKEN:-}"

if [ -z "$token" ] && [ "$FROM_GH" -eq 1 ]; then
  echo "⚠️  --from-gh-token reuses your personal gh CLI token as the repo secret."
  echo "    It carries ALL your gh scopes and rotates when you re-auth gh. A scoped"
  echo "    fine-grained/classic PAT (repo + workflow only) is the safer choice."
  printf "Proceed anyway? [y/N] "
  read -r reply
  case "$reply" in
    y | Y | yes | YES) token="$(gh auth token)" ;;
    *)
      echo "Aborted."
      exit 1
      ;;
  esac
fi

if [ -z "$token" ]; then
  url="https://github.com/settings/tokens/new?scopes=repo,workflow&description=release-plz%20(${REPO##*/})"
  echo "Create a classic PAT with the 'repo' + 'workflow' scopes (pre-filled):"
  echo "  $url"
  open_url "$url"
  printf "Paste the token (input hidden), then Enter: "
  # -s is a bash extension; it works in Git Bash but not under `sh script.sh`.
  # Fall back to a visible prompt rather than failing to read anything at all.
  if read -rs token 2>/dev/null; then
    echo
  else
    read -r token
  fi
fi

[ -n "$token" ] || {
  echo "no token provided — aborting." >&2
  exit 1
}

# Strip surrounding whitespace and any CR. A token pasted into a Windows
# terminal, or read from a CRLF-ended env file, otherwise gets stored with a
# trailing \r — and the secret then fails authentication with no useful error,
# which is the worst possible way for this to go wrong.
token="$(printf '%s' "$token" | tr -d '\r\n' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"

[ -n "$token" ] || {
  echo "the token was empty after trimming whitespace — aborting." >&2
  exit 1
}

# ── Set it (value via stdin, never argv) ─────────────────────────────────────
printf '%s' "$token" | gh secret set "$SECRET" --repo "$REPO"

if secret_exists; then
  echo "✓ ${SECRET} configured on ${REPO}."
  echo "  release-plz will now push tags that trigger release.yml (binaries + npm)."
else
  echo "gh reported success but ${SECRET} does not exist on ${REPO} — check" >&2
  echo "  gh api repos/${REPO}/actions/secrets/${SECRET}" >&2
  exit 1
fi
