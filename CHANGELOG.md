# Changelog

All notable changes to CacheFerret are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/rvben/cacheferret/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/rvben/cacheferret/releases/tag/v0.1.0
