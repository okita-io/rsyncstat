# AGENTS.md

Guidance for AI agents (and humans) working in this repo. Read this first.

## What this is

RSyncStat is a native Rust desktop app (eframe/egui + wgpu): a WinDirStat-style treemap of two folders plus a safe one-way copy from A to B. Design and roadmap: `docs/SPEC.md`. Public overview: `README.md`.

## Commands

Run these before finishing any change. CI runs the same on Linux, macOS and Windows.

```bash
cargo fmt --all
cargo clippy --workspace --all-targets   # must stay at zero warnings
cargo test --workspace --locked
cargo build --workspace --locked
cargo run --release -p rsyncstat-app     # launch the GUI
```

Rust stable 1.95+ (`rust-toolchain.toml`). Linux needs GTK/xkb dev packages, see the README.

## Where code goes

| Crate | Owns | Must not |
|-------|------|----------|
| `rsyncstat-model` | Tree types, file colors, byte/path formatting | depend on other workspace crates |
| `rsyncstat-filter` | Exclude presets, named YAML packs, gitignore-style rules | touch the file system |
| `rsyncstat-scan` | Parallel directory walk, streaming results | know about the UI |
| `rsyncstat-treemap` | Squarified layout, hit testing | know about egui |
| `rsyncstat-plan` | Compare trees, decide what to copy (pure logic) | touch the file system |
| `rsyncstat-exec` | Run a plan: atomic copy, hash check, retries, cancel | decide what to copy |
| `rsyncstat-app` | Window, input, painting; wires the crates together | hold sync or scan logic |

Inside `rsyncstat-app/src/`:
- `app/mod.rs` holds window state. Behavior is split by concern: `scan_ctl` (folders, scans), `sync_ctl` (compare, plan, sync), `input` (keyboard), `frame` (layout), `pane`, `pane_view`.
- `ui/` holds widgets: `treemap`, `tree_view`, `legend`, `dither`, `diff`, `patterns`.
- App icons: edit the master `docs/images/icon-original.png`, then run `scripts/make-icons.sh` to regenerate `assets/icon.{png,ico,icns}` and the README logo.
- `keyboard.rs`, `files.rs`, `volume.rs`, `platform.rs` are standalone helpers.

Put new logic in a library crate with tests, then call it from the app. Keep changes inside one module where you can, so parallel agents do not collide. Dependencies flow downward only; do not add cycles.

## Safety rules (do not weaken)

- Sync never deletes and never overwrites. An existing target file is hashed: same is skipped, different is reported and left alone.
- Copies go to a `.rsyncstat-tmp` file, then rename. Never leave temp files behind.
- Relative paths are validated (`safe_rel`), and writes through a symlinked directory on the target are refused. Keep both guards and their tests.
- Excluded paths are shown but never scanned into or copied.
- Anything that removes user files must go through the Trash (`trash` crate) after an explicit confirmation.
- Tests must not touch the real home directory. `execute` does no global I/O; `spawn_sync` takes the mismatch-list path as a parameter (`None` in tests).

## Testing

- Unit tests sit next to the code. End-to-end tests with real temp directories are in `crates/*/tests/` (`rsyncstat-exec/tests/e2e_sync.rs`, `rsyncstat-scan/tests/e2e_scan.rs`, `rsyncstat-app/tests/e2e_treemap.rs`).
- New behavior needs a test; a bug fix needs a regression test. Use the `Sandbox` pattern in the e2e files for temp dirs.
- Keyboard mnemonics must stay unique per view; tests in `keyboard.rs` enforce it.

## Conventions

- Conventional Commits: `feat(app): ...`, `fix(exec): ...`, `docs: ...`, `test(scan): ...`, `refactor: ...`.
- Keep PRs and commits focused. Update `CHANGELOG.md` for user-visible changes and the README when crates or features change.
- Commit `Cargo.lock`. Use the windows-crate pin described in the README if `cargo update` breaks Windows.
- No dead code, compatibility shims or placeholder crates. Add a crate when its milestone actually starts.
- Match the existing style; do not reformat unrelated code.
- Never commit secrets, `.DS_Store` or `target/`.
