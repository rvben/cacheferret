# Changelog

All notable changes to CacheFerret are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0](https://github.com/rvben/cacheferret/compare/v0.4.2...v0.5.0) - 2026-08-27

### Added

- TUI scans now show recognized caches as soon as they are discovered and
  replace sizing rows with current measurements as background work completes.
- A private warm-start scan index shows previously known paths immediately,
  prioritizes their fresh measurement, and never allows stale data to authorize
  cleanup.
- Docker storage inspection now reports images, containers, volumes, and build
  cache as separate daemon-scoped resources in the CLI, JSON, schema, and TUI.
- Ordinary Docker build cache can now be selected in the TUI or pruned with
  `cacheferret docker clean` after a fresh preview and mandatory confirmation.
  Images, containers, volumes, broad system prune, and `builder prune --all`
  remain outside the cleanup boundary.
- Post-release smoke tests now install and execute the exact published version
  from GitHub archives, crates.io, PyPI, and Homebrew on native Intel/ARM macOS
  and Linux runners.
- A native cleanup adapter contract now defines when daemon and package-manager
  pruning is safer than direct directory deletion, beginning with Docker.

### Fixed

- Warm-start rows keep their unavailable action explanation visible in compact
  layouts until fresh measurement makes selection safe.
- The Homebrew formula now uses Homebrew's architecture-aware DSL so ARM Linux
  selects its published native archive instead of being rejected as unsupported.

## [0.4.2](https://github.com/rvben/cacheferret/compare/v0.4.1...v0.4.2) - 2026-08-26

### Changed

- Vership now updates durable documentation version references inside each
  release commit and refuses to tag until all GitHub publishing secrets exist.
- GitHub workflows now use current Node 24 action releases and run strict lint,
  Linux end-to-end safety, packaging, and crate checks before publication.

### Fixed

- Registry and Homebrew jobs now fail closed instead of reporting success while
  silently skipping publication when credentials are absent.

## [0.4.1](https://github.com/rvben/cacheferret/compare/v0.4.0...v0.4.1) - 2026-08-26

### Fixed

- Linux builds now use a platform-correct `statvfs` block-count conversion, and
  the Linux end-to-end suite recognizes the expanded cleanup size columns.

## [0.4.0](https://github.com/rvben/cacheferret/compare/v0.3.1...v0.4.0) - 2026-08-26

### Added

- Allocated-block estimates alongside apparent directory sizes in scans,
  confirmations, dry runs, cleanup summaries, TUI details, and JSON output.
- Per-filesystem before/after free-space measurements for completed cleanups,
  including signed net deltas and explicit multi-filesystem reporting.

### Changed

- Hard-linked files are counted once during tree measurement.
- Storage copy now distinguishes apparent size, allocated-block estimates, and
  observed disk-free changes without claiming APFS shared blocks were reclaimed.

### Fixed

- clispec command examples now contain arguments relative to their command, so
  automated conformance checks execute valid invocations instead of duplicating
  the command name.

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
