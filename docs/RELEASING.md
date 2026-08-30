# Releasing think-and-ship

> **How-to** — steps for one task, assuming a working install ([all docs](README.md)).

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

Two properties keep step 2 honest without a human in the loop:

- **`main` is linear.** Pull requests are squash-merged (the repository allows
  no other method; the ruleset requires linear history) with the PR title as
  the commit subject, so every commit on `main` is one Conventional Commit.
  release-plz finds the previous release by walking history back to where the
  version last changed — a merge commit whose side branch was cut before that
  release (a dependabot branch, typically) stops the walk early and drops
  changelog entries. v0.5.1 lost its `fix(roadmap)` exactly this way.
- **The npm version is the tag.** `npm/think-and-ship/package.json` carries the
  placeholder `0.0.0-development`; `release.yml` stamps the published package
  from the `v*` tag. There is no committed npm version to bump, sync or drift
  (v0.5.0 and v0.5.1 each needed a hand-made bump when there was one).
- **The release PR's changelog is complete.** After release-plz updates it, the
  same job runs `scripts/release/sync-release-pr.py` on the PR branch and
  appends any `feat`/`fix` commit since the last tag that the generated
  section lacks. The fix-up is pushed with `RELEASE_PLZ_TOKEN`, so the PR's CI
  re-runs. `scripts/release/sync-release-pr.py --check` is the dry run.

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
| `SHA256SUMS` over all release assets | release.yml | `release.yml` `release` job; verified by `npm/think-and-ship/install.js` |

While the repository is private, the OpenSSF Scorecard workflow
(`scorecard.yml`) skips itself — the action needs a public repository. To
learn the current score anyway: `brew install scorecard`, then
`GITHUB_AUTH_TOKEN=$(gh auth token) scorecard --repo=github.com/AlrikOlson/think-and-ship`.

`cargo publish`. `think-and-ship-viewer` is `publish = false` (Tauri app, not
a crates.io package).

## ⚠️ Required one-time setup (USER ACTIONS)

These secrets/config must exist on the GitHub repo before the automation works:

- [ ] **`RELEASE_PLZ_TOKEN` secret** — a fine-grained PAT (or GitHub App token)
  with `contents: write` + `pull-requests: write`. **The whole cascade rests on
  this:** GitHub does **not** fire downstream workflows for events created with
  the default `GITHUB_TOKEN` (anti-recursion rule). Without this PAT, release-plz
  pushes the tag but **release.yml never runs** — the release ends up with a
  changelog and zero binaries (this is exactly what happened to v0.2.0 and
  v0.3.0, and it is why npm sat on 0.1.1 while crates.io reached 0.3.0).

  Both release-plz jobs used to fall back to `GITHUB_TOKEN` when the secret was
  absent, which published a half-release and reported success. **They now stop
  with an explanatory error instead**, before anything reaches crates.io — a
  crates.io publish is permanent, so failing afterwards is not a recovery.
  - **Quick setup (idempotent):** opens the pre-filled PAT page, then sets the
    secret via `gh`. Re-running is a no-op once the secret exists; `--force` /
    `-Force` replaces it.

    | Platform | Command | Non-interactive |
    |---|---|---|
    | macOS, Linux | `bash docs/deploy/setup-release-plz-token.sh` | `RELEASE_PLZ_TOKEN=ghp_xxx bash docs/deploy/...sh` |
    | Windows | `.\docs\deploy\setup-release-plz-token.ps1` | `.\docs\deploy\...ps1 -Token ghp_xxx` |

    Use the `.ps1` on Windows. The `.sh` runs under Git Bash, but `bash` in
    PowerShell can resolve to WSL's bash depending on PATH, which pulls a whole
    Linux environment into a two-command task.

  - **Or skip the script.** It only automates a browser open and one `gh` call.
    `gh secret set RELEASE_PLZ_TOKEN` prompts for the value with hidden input
    and works identically from any shell.
- [ ] **crates.io Trusted Publishing** — no `CARGO_REGISTRY_TOKEN` secret. The
  `release-plz release` job mints an OIDC token (`id-token: write`), exchanges
  it via `rust-lang/crates-io-auth-action` for a short-lived crates.io token,
  and that action revokes it in its post step. Configure it on crates.io under
  the crate's Settings → Trusted Publishing:

  | Field | Value |
  |---|---|
  | Publisher | GitHub |
  | Repository owner | `AlrikOlson` |
  | Repository name | `think-and-ship` |
  | Workflow filename | **`release-plz.yml`** |
  | Environment name | *(blank)* |

  The workflow filename is the one that runs `cargo publish` — `release-plz.yml`,
  **not** `release.yml`. `release.yml` only builds binaries and publishes to npm,
  and naming it here fails the OIDC audience check at publish time.
- [x] **npm Trusted Publishing** — done, and there is **no `NPM_TOKEN` secret**.
  `publish-npm` authenticates with OIDC (`id-token: write`).

  Note the asymmetry, because guessing it wrong fails at publish time with a
  bare `E404 ... or you do not have permission`, which reads like a missing
  package rather than a rejected credential:

  | Registry | Trusted publisher names |
  |---|---|
  | crates.io | `release-plz.yml` — the workflow that runs `cargo publish` |
  | npm | `release.yml` — the workflow whose `publish-npm` job runs `npm publish` |

  A failed npm auth still signs and logs a provenance statement first, since
  that step only needs the GitHub OIDC identity. A signed provenance line in
  the log is not evidence the publish succeeded.
- [x] **README npm caveat** — removed; `Registry parity` is green at 0.4.0.

### How divergence is prevented now

Two gates, because the failure was silent in two different places:

| Gate | Where | Catches |
|---|---|---|
| tag-stamped npm version | `release.yml` `publish-npm` | a committed npm version drifting from Cargo.toml — there is none to drift; the published version is the tag |
| `Registry parity` | `registry-parity.yml`, daily + on version-file changes | crates.io and npm serving different versions |

Plus two things that can no longer silently degrade: release-plz **fails** when
`RELEASE_PLZ_TOKEN` is missing rather than falling back, and `publish-npm`
sets the package version from the **release tag** rather than trusting the
committed `package.json`.

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

## Backfilling a release that has no binaries

If a tag was created but release.yml never ran (the missing-PAT case above),
dispatch release.yml against the existing tag — every job checks out that tag,
the artifacts are named after it, and the attach step upserts that tag's
release without touching its changelog body:

```sh
gh workflow run release.yml -f tag=v0.3.0
```

One dispatch runs the full cascade for that tag: five-target binaries
(including the Windows `.exe`), `SHA256SUMS`, the npm publish, and the frozen
crates.io stubs (no-ops if already published). npm refuses a duplicate
version, so re-dispatching a tag whose npm version already shipped will fail
the `publish-npm` job — that is the guard, not a bug.

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
