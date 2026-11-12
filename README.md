# RSyncStat

RSyncStat is a native Rust application that combines a WinDirStat-style disk map with two-way folder sync (planned). **M0** (this branch) is a spike: scan one local folder with gitignore-style exclude presets, show a squarified treemap, directory tree, file-type legend, and hatched tiles for excluded paths. No sync yet.

The full product spec lives in [docs/SPEC.md](docs/SPEC.md).

## Workspace layout

| Crate | M0 status |
|-------|-----------|
| `rsyncstat-model` | Scan tree types |
| `rsyncstat-filter` | Built-in exclude presets (`ignore` gitignore builder) |
| `rsyncstat-scan` | Parallel walk, prunes excluded directories |
| `rsyncstat-treemap` | Squarified layout + hit testing |
| `rsyncstat-app` | eframe/egui 0.37 + wgpu desktop UI |
| `rsyncstat-index`, `reconcile`, `plan`, `exec`, `transport` | Stubs |
| `rsyncstat-agent` | Stub binary (M3) |

## Prerequisites

- Rust **stable** (see `rust-toolchain.toml`; 1.88+ recommended for current `ignore` / `eframe` releases)
- Platform libraries for the GUI (see below)

## Build

From the repository root:

```bash
cargo build --release -p rsyncstat-app
```

The binary is `target/release/rsyncstat` (or `rsyncstat.exe` on Windows).

## Run

```bash
cargo run -p rsyncstat-app
```

Use **Open folder…** to pick a root. The scanner uses all CPU cores, applies default exclude presets (including `*.lancedb`, `node_modules/`, `target/`, `.venv/`, system junk), and does not descend into excluded directories. Excluded entries appear in the tree and treemap with a hatch pattern.

## Platform notes

### macOS

- Install Xcode command line tools if Rust was not installed via `rustup`.
- First run may prompt for folder access depending on what you scan.
- Build: `cargo build --release -p rsyncstat-app`
- Hyprland-style keyboard issues do not apply on macOS.

### Windows

- Use a recent Visual Studio Build Tools install (for the MSVC linker) if `rustup` did not configure it.
- Long paths: the scanner uses the standard library; future milestones will use verbatim `\\?\` paths where needed.
- Run: `target\release\rsyncstat.exe`

### Linux (including Arch / Omarchy / Hyprland)

On Arch:

```bash
sudo pacman -S --needed base-devel pkgconf gtk3 libxkbcommon wayland
```

On Debian/Ubuntu:

```bash
sudo apt install libgtk-3-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev
```

**Hyprland (Wayland):** M0 must be exercised on a real Hyprland session. There is an open egui report that keyboard input is ignored on Hyprland ([#8249](https://github.com/emilk/egui/issues/8249)); confirm focus and typing in text fields after upgrading to eframe 0.37. A related Wayland repaint stall on hidden workspaces was addressed in 0.37 ([#5136](https://github.com/emilk/egui/issues/5136)).

Typical Hyprland launch (from a built tree):

```bash
cargo run -p rsyncstat-app
```

Ensure `WLR_NO_HARDWARE_CURSORS=1` only if your compositor docs require it for fractional scaling; not usually needed for a smoke test.

## Tests

```bash
cargo test -p rsyncstat-filter -p rsyncstat-treemap
cargo test --workspace
cargo build --workspace
```

CI runs the unit tests and a full workspace build on Ubuntu, macOS, and Windows.

## License

MIT — see [LICENSE](LICENSE).
