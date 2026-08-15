## What and why

<!-- One or two sentences: the change, and the problem it fixes. -->

## Checks

- [ ] `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all pass locally
- [ ] Behavioural changes come with a test that fails without them
- [ ] Commit subjects follow Conventional Commits and pass the `commit-msg` hook (`just hooks` to install — see CONTRIBUTING.md)
