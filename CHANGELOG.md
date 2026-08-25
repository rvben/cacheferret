# Changelog

All notable changes to CacheFerret are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/rvben/cacheferret/compare/v0.3.0...v0.3.1) - 2026-08-25

### Fixed

- Removed the unsupported `--locked` argument from the PyPI source-distribution
  job so current Maturin releases can build and publish the sdist.

## [0.3.0](https://github.com/rvben/cacheferret/compare/v0.2.1...v0.3.0) - 2026-08-25

### Added

- Conservative macOS temporary-storage discovery for abandoned Chrome
  code-signing clones, recognized developer build caches under `/private/tmp`,
  and large temporary project workspaces.
- Scan-only reporting for temporary workspaces that may contain unique work,
  plus final change detection before deleting volatile temporary caches.
- An explicit TUI override for individually selected scan-only entries, with a
  mandatory confirmation and final safety revalidation.
- Truthful TUI cleanup wording and an APFS-clone size warning, avoiding a claim
  that apparent directory size equals physically reclaimed disk space.

## [0.2.1](https://github.com/rvben/cacheferret/compare/v0.2.0...v0.2.1) - 2026-08-18

### Added

- Fast TUI batch selection with `Space` to toggle and advance, `a` to toggle all
  visible deletable caches, and `d` to delete the selection with one proportional
  confirmation.

### Fixed

- Removed the duplicated scanning status from the TUI header.

## [0.2.0] - 2026-08-18

### Added

- A responsive, keyboard-first TUI with background scanning, filtering, sorting,
  scope views, detailed cache inspection, direct `d`-key deletion, and compact
  confirmation for risky targets.
- Adaptive truecolor, 256-color, no-color, ASCII, and reduced-motion terminal
  modes, with rendering regressions and PTY lifecycle coverage on macOS and Linux.
- Native PyPI wheels, so the same executable can be installed with
  `pipx install cacheferret`.

### Changed

- Cache size and activity are remeasured in the background whenever `d` is
  pressed, so confirmation decisions use current filesystem state.
- Ratatui and Crossterm were upgraded to their current release lines, removing
  the obsolete `paste` dependency and updating `lru` beyond its audited releases.

## [0.1.0] - 2026-08-17

### Added

- Read-only discovery and parallel sizing of developer caches on macOS and Linux.
- A closed catalog spanning 53 cache kinds across 17 ecosystems.
- Project, global, age, kind, pagination, and field-selection controls.
- Dry-run cleanup plans with per-target size and restore requirements.
- Interactive confirmation and explicit `--yes` automation support.
- Filesystem identity, containment, ownership-marker, and symlink safety checks.
- Scan-only policy for Maven and renv stores that may hold irreplaceable state.
- Structured JSON, stable errors, shell completions, and clispec.dev v0.3 introspection.
- Release archives for Intel and ARM macOS and Linux, plus Homebrew automation.
