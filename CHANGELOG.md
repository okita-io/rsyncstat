# Changelog

All notable changes are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project aims to follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Named include/exclude packs (git, node, vite, rust, python) from a chevron menu, stored in `include-exclude.yaml` next to the app
- Parallel scanner with gitignore-style exclude presets and hatched excluded tiles
- Squarified treemap, directory tree and file-type legend
- Two-pane layout with per-pane folder controls
- Keyboard navigation, Alt mnemonics, shortcut cheat sheet and file menu
- Dithered file map on a shared pixel grid
- Community files, issue and PR templates, Dependabot

### Added (icons)
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
- The left panel's edge line ran down through the status bar when the file types section had no room left
- Folder and include/exclude field text sat on the left stroke because custom frames dropped TextEdit padding
- A symlinked directory on the target could redirect a copy outside the target root
