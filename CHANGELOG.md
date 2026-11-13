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

### Changed
- Sync logic moved out of the app into `rsyncstat-plan` and `rsyncstat-exec`
- App split into `app/` and `ui/` modules
- Byte formatting shared through `rsyncstat-model`

### Removed
- Unused stub crates (index, reconcile, transport, agent) and unused workspace dependencies
- Unused `PresetGroup`, `NodeId`, `should_prune_dir` and placeholder rule levels

### Fixed
- A symlinked directory on the target could redirect a copy outside the target root
