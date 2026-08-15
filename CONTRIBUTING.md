# Contributing

Three commands decide whether a change merges. Run them before pushing;
CI runs the same ones and nothing softer:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI additionally runs clippy and the test suite on Linux, macOS and Windows
(with `--workspace --all-targets --exclude think-and-ship-viewer` — the
viewer crate needs a Tauri toolchain the runners do not install), plus
rustdoc and an MSRV check. The three commands above are the whole local gate.

The minimum supported Rust version is **1.89** (`rust-version` in
`crates/think-and-ship/Cargo.toml`). Clippy and rustfmt come from the
stable toolchain.

## One-time setup

```sh
just hooks
```

This points `core.hooksPath` at the tracked `.githooks/`, which installs a
`commit-msg` hook. The hook rejects commit subjects that narrate the
project's internal development process — phase numbers, internal ticket
ids, reasoning-trace references. The rule it enforces is
[`docs/STYLE.md`](docs/STYLE.md) S8. Commits from before the hook existed
keep their subjects as-is; that decision and its reasons are recorded in
S8 — cite it rather than reopening it.

## Commit messages

Subjects follow [Conventional Commits](https://www.conventionalcommits.org)
(`feat:`, `fix:`, `docs:`, `test:` …). release-plz derives version bumps
and the changelog from them, so the prefix is load-bearing, not
decorative. The `commit-msg` hook checks the S8 rule; it does not check
the prefix — CI and review do.

## Prose

Documentation changes follow [`docs/STYLE.md`](docs/STYLE.md). It is
written as numbered rules so review can cite a rule instead of arguing
taste. The short version: lead with the fact, one claim per sentence, no
process narration, define a term once at first use.

## Tests

Unit tests live in the same file under `#[cfg(test)]`; integration tests
in `tests/`. A behavioural fix should come with the test that fails
without it. Tests that assert over source text (the repository has
several such gates) must be proven against a sabotaged tree before they
count — a gate that has never been red proves nothing.

## Maintenance capacity

This project is maintained by one person. Issues and pull requests are
read, but triage may be slow and there is no response-time commitment.
Small, focused pull requests that pass the three gates are the fastest
path to a merge.
