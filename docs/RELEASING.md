# Releasing think-and-ship

This project releases from CI on every merge to `main`, driven by
[Conventional Commits](https://www.conventionalcommits.org/). You almost never
run a publish command by hand — you merge a PR.

## TL;DR

1. Land normal work on `main` with Conventional Commit messages
   (`feat:`, `fix:`, `docs:`, `chore:`, …).
2. **release-plz** keeps an open **"Release vX.Y.Z" PR** that bumps the
   `think-and-ship` version and updates `CHANGELOG.md`.
3. **Merge that PR.** release-plz then publishes `think-and-ship` to crates.io,
   pushes the `vX.Y.Z` tag, and creates the GitHub release with the changelog.
4. The tag triggers **release.yml**, which builds the five binaries, attaches
   them to the GitHub release, publishes the npm package, and re-publishes the
   frozen crates.io stubs (no-op unless their versions changed).

## Who owns what

| Concern | Owner | Where |
|---------|-------|-------|
| Version bump (SemVer from commits) | release-plz | `release-plz.toml`, `release-plz.yml` |
| `CHANGELOG.md` generation | release-plz | `release-plz.toml` `[changelog]` |
| crates.io publish of `think-and-ship` | release-plz | `release-plz.yml` (`command: release`) |
| `vX.Y.Z` git tag | release-plz | `release-plz.yml` |
| GitHub release + changelog body | release-plz | `git_release_enable = true` |
| Cross-platform binaries (5 targets) | release.yml | `release.yml` `build` job |
| Attaching binaries to the release | release.yml | `release.yml` `release` job (upsert by tag) |
| npm publish (`think-and-ship`) | release.yml | `release.yml` `publish-npm` |
| crates.io publish of the **stubs** | release.yml | `release.yml` `publish-crates-stubs` |

The two frozen deprecation stubs (`deliberate-mcp`, `resolute-mcp`) are
`release = false` in `release-plz.toml`, so release-plz never bumps or
publishes them — their versions only change by a deliberate manual
`cargo publish`. `think-and-ship-viewer` is `publish = false` (Tauri app, not
a crates.io package).

## ⚠️ Required one-time setup (USER ACTIONS)

These secrets/config must exist on the GitHub repo before the automation works:

- [ ] **`RELEASE_PLZ_TOKEN` secret** — a fine-grained PAT (or GitHub App token)
  with `contents: write` + `pull-requests: write`. **This is load-bearing:**
  GitHub does **not** fire downstream workflows for events created with the
  default `GITHUB_TOKEN` (anti-recursion rule). Without this PAT, release-plz
  pushes the tag but **release.yml never runs** — you'd have to delete and
  re-push the tag by hand to build binaries. The workflows fall back to
  `GITHUB_TOKEN` if the secret is absent (publish still happens; the cascade
  does not).
- [ ] **`CARGO_REGISTRY_TOKEN` secret** — crates.io API token (already needed by
  the old flow).
- [ ] **`NPM_TOKEN` / npm provenance** — npm publish uses OIDC
  (`id-token: write`) in `publish-npm`; confirm the npm package is configured
  for trusted publishing or add a token.

## Homebrew tap (USER ACTION)

Full `brew install alrikolson/tap/think-and-ship` support needs a separate tap
repo that this repo can't create for you:

1. Create `AlrikOlson/homebrew-tap` on GitHub.
2. Copy `docs/deploy/homebrew/think-and-ship.rb` to `Formula/think-and-ship.rb`
   in that repo.
3. After each release, update the `version` and the four `sha256` values to
   match the new release tarballs:
   ```sh
   V=0.3.0
   for t in aarch64-apple-darwin x86_64-apple-darwin \
            aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
     url="https://github.com/AlrikOlson/think-and-ship/releases/download/v$V/think-and-ship-v$V-$t.tar.gz"
     echo "$t: $(curl -sL "$url" | shasum -a 256 | cut -d' ' -f1)"
   done
   ```
4. Commit + push the tap repo.

> Automating the tap bump (a `brew-tap-update` job that opens a PR against the
> tap repo on each release) is tracked in the roadmap backlog — it needs the
> tap repo + a cross-repo token first.

## Conventional Commit → changelog mapping

`release-plz.toml` `[changelog]` maps commit prefixes to Keep-a-Changelog
sections: `feat:` → **Added**, `fix:` → **Fixed**, `perf:`/`refactor:` →
**Changed**, `docs:` → **Documentation**, security-flagged bodies → **Security**.
`chore:`/`test:`/`ci:` are skipped. A `!` breaking-change marker (e.g.
`feat!:`) drives a major/minor bump per SemVer.

## Manual / emergency release

If you must release without the PR flow (e.g. CI is down):

```sh
# Dry-run to see what release-plz would do:
release-plz update --dry-run

# Publish + tag locally (needs CARGO_REGISTRY_TOKEN in env):
release-plz release
git push --tags   # only triggers release.yml if pushed with a PAT-backed remote
```
