# CacheFerret project memory

## Working agreements

- Use Conventional Commits: `<type>[optional scope][!]: <description>`.
- Preserve unrelated user changes and keep release work reproducible.
- Treat `docs/product.md` as the durable product and UX direction.
- Keep README claims, `cacheferret schema`, and actual behavior aligned.

## What CacheFerret is

CacheFerret is a Rust CLI and keyboard-first TUI for finding, understanding,
and removing rebuildable developer caches on macOS and Linux. It is a disk
space tool for developers, not a general-purpose filesystem cleaner.

The product has two equally important faces:

- For humans, bare `cacheferret` opens a fast interactive cache workspace.
- For scripts and agents, piped output is structured JSON and the command
  surface follows clispec.dev v0.3.

The interaction model should feel familiar to ncdu/gdu users: `Space` toggles a
cache and advances, `a` toggles all deletable caches in the visible view, and
`d` deletes the selection (or the focused item when there is no selection). Do
not add blanket confirmation or ceremony to every delete. Ask for one compact
`y`/`n` confirmation for the operation only when a freshly measured target is
risky: recent, at least 1 GiB, shared/global, unknown-age, or dependent on a
package download for restoration. Catalog entries marked scan-only must never
be selectable or deletable.

## Product principles learned in the 0.2.0 session

1. Directness is part of safety. Keep `d` as an immediate focused-item action,
   while also preserving rapid `Space`/`a` batch selection for larger cleanups.
2. Guardrails should be proportional and contextual. Low-risk cache deletion
   should be one keypress; risky deletion should require one explicit answer.
3. Safety checks belong close to mutation, not as repeated user friction.
   Pressing `d` remeasures the whole operation in the background, and deletion
   revalidates each path,
   filesystem identity, containment, kind, ownership markers, and symlink
   policy immediately before removal.
4. The TUI is the product experience, not a decorative wrapper around the CLI.
   Preserve responsive scanning, useful empty/loading/error states, filtering,
   sorting, scope switching, details, compact help, and readable status copy.
5. Terminal diversity is a supported product constraint. Keep truecolor,
   256-color, ANSI, `NO_COLOR`, ASCII, non-UTF-8, reduced-motion, narrow-window,
   and PTY lifecycle behavior working.
6. Machine-readable behavior is a first-class interface. Human-facing TUI work
   must not weaken JSON output, stable errors, schema introspection, pagination,
   field selection, stdout/stderr separation, or non-interactive confirmation.
7. Be precise about what is rebuildable. Maven's local repository and the
   shared renv cache are scan-only. Docker storage uses native inspection;
   ordinary build cache alone has guarded native pruning, while images,
   containers, volumes, and broader build records remain inspection-only.
8. Avoid repeating the same state label within one region. During scanning the
   header should have one status label; supporting detail belongs in the body.

## Current release state

- Current version: `0.5.0`.
- Published to crates.io: <https://crates.io/crates/cacheferret>
- Published to PyPI: <https://pypi.org/project/cacheferret/>
- PyPI provides native x86_64/aarch64 wheels for macOS and manylinux2014 Linux,
  plus an sdist. Windows is not currently a supported wheel target.
- Vership verification confirms the current tag, GitHub release, crates.io,
  PyPI, and Homebrew; PyPI exposes all five expected artifacts.
- Source, tags, release archives, and checksums are public at
  <https://github.com/rvben/cacheferret/releases/latest>.
- Tag-driven release workflows fail closed when a required publishing secret is
  missing; Vership checks the configured secret names before creating a tag.
- Never copy local Cargo or PyPI credentials into GitHub secrets without the
  user's explicit authorization. Registry publishing can be performed locally;
  releases must stop rather than silently skipping that publishing target.

## Release and quality baseline

Before release, run the documented checklist in `docs/releasing.md`. The current
baseline includes:

- `make check`: formatting, Clippy, packaging syntax, and the full test suite.
- `make conformance`, Cargo package verification, and Maturin sdist/wheel builds.
- strict Twine checks for all five Python artifacts.
- post-release install smoke tests across GitHub archives, crates.io, PyPI, and
  Homebrew on native Intel/ARM macOS and Linux runners.
- final `vership verify` across the tag, GitHub release, crates.io, PyPI, and
  Homebrew.

Python packaging uses Maturin binary bindings; it distributes the same Rust
executable rather than a Python reimplementation. Keep Cargo and Python package
versions synchronized.
