# Contributing to RSyncStat

Thanks for helping. Please open an issue before starting large changes so we can agree on direction. The design is in [docs/SPEC.md](docs/SPEC.md).

## Setup

1. Install Rust stable 1.95 or newer (`rust-toolchain.toml` pins the channel).
2. Install the platform GUI libraries listed in the [README](README.md#platform-notes).
3. `cargo build --workspace`

## Before you open a PR

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --workspace --locked
```

CI runs the format check, tests and build on Linux, macOS and Windows.

## Where to work

Crates and app modules have narrow responsibilities (see the README table), so separate changes rarely touch the same files. Logic goes in a library crate with tests; `rsyncstat-app` should only wire it to the UI. End-to-end tests live in `crates/*/tests/`.

## Guidelines

- Keep PRs focused; one logical change each.
- Add tests for logic in `model`, `filter`, `scan`, `treemap`, `plan` and `exec`.
- Keyboard mnemonics must stay unique per view; the tests in `keyboard.rs` enforce it.
- Commit Cargo.lock changes when dependencies change.
- Use [Conventional Commits](https://www.conventionalcommits.org/) (`feat(app): ...`, `fix(scan): ...`).
- Sync code must never delete without going through the trash and an approved plan.

By contributing you agree your work is released under the [MIT License](LICENSE).
