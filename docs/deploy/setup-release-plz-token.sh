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
# Usage:
#   docs/deploy/setup-release-plz-token.sh                 # interactive: opens the PAT page, paste it
#   RELEASE_PLZ_TOKEN=ghp_xxx docs/deploy/setup-release-plz-token.sh   # non-interactive (CI)
#   docs/deploy/setup-release-plz-token.sh --from-gh-token # reuse your gh CLI token (see warning)
#   docs/deploy/setup-release-plz-token.sh --force         # replace an existing secret
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

secret_exists() {
  gh secret list --repo "$REPO" 2>/dev/null | grep -q "^${SECRET}[[:space:]]"
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
  if command -v open >/dev/null 2>&1; then
    open "$url" >/dev/null 2>&1 || true
  elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$url" >/dev/null 2>&1 || true
  fi
  printf "Paste the token (input hidden), then Enter: "
  read -rs token
  echo
fi

[ -n "$token" ] || {
  echo "no token provided — aborting." >&2
  exit 1
}

# ── Set it (value via stdin, never argv) ─────────────────────────────────────
printf '%s' "$token" | gh secret set "$SECRET" --repo "$REPO"

if secret_exists; then
  echo "✓ ${SECRET} configured on ${REPO}."
  echo "  release-plz will now push tags that trigger release.yml (binaries + npm)."
else
  echo "gh reported success but ${SECRET} is not listed — check 'gh secret list --repo ${REPO}'." >&2
  exit 1
fi
