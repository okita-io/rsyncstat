# Changelog

All notable changes are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project aims to follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-11-14

### Added
- Omarchy theme: window chrome follows `~/.local/state/omarchy/current/theme/colors.toml` (background, foreground, selection, muted, accent, and status colors) and reloads when the theme changes. On Hyprland, button, field, menu and focus corners follow `decoration:rounding`. Treemap file colors stay the per-extension palette
- Arch package files in `pkgbuild/` (desktop entry, hicolor icon, license, `/usr/bin/rsyncstat`) and packaging notes in `docs/packaging.md`. The Wayland app id is `rsyncstat`
- Named include/exclude packs (git, node, vite, rust, python) from a chevron menu, stored in `~/.config/rsyncstat/include-exclude.yaml` (or `%APPDATA%\rsyncstat\` on Windows). An existing portable file beside the binary is still used
- Parallel scanner with gitignore-style exclude presets and hatched excluded tiles
- Squarified treemap, directory tree and file-type legend
- Two-pane layout with per-pane folder controls
- Keyboard navigation, Alt mnemonics, shortcut cheat sheet and file menu
- Dithered file map on a shared pixel grid
- Community files, issue and PR templates, Dependabot
- `scripts/bundle-macos.sh` builds, signs (hardened runtime) and optionally notarizes the macOS app and a drag-to-Applications DMG
- Windows exe icon (build script), macOS `.app` bundling metadata for cargo-bundle, and a tag-triggered release workflow
- App icon, plus `scripts/make-icons.sh` to build PNG, ICO and ICNS files from one master PNG

### Changed
- Source, Target and Run sync share one evenly spaced row; Tab order now runs row by row across the window (Run sync → Source… → Target… before the filters)
- Source and Target tabs are framed toggles that match the other buttons in light and dark mode
- The first row of buttons starts at the same height in the left panel and both panes
- Sync logic moved out of the app into `rsyncstat-plan` and `rsyncstat-exec`
- App split into `app/` and `ui/` modules
- Byte formatting shared through `rsyncstat-model`

### Removed
- Unused stub crates (index, reconcile, transport, agent) and unused workspace dependencies
- Unused `PresetGroup`, `NodeId`, `should_prune_dir` and placeholder rule levels

### Fixed
- Directory tree rows were double spaced (the 30 px control height plus the focus gap); each row is now one line of text
- The left panel's edge line ran down through the status bar when the file types section had no room left
- Folder and include/exclude field text sat on the left stroke because custom frames dropped TextEdit padding
- A symlinked directory on the target could redirect a copy outside the target root
- Folder dialog on Linux runs off the UI thread so the window stays responsive while the portal is open
