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

The interaction model should feel familiar to ncdu/gdu users: focus an item and
press `d` to delete it. Do not add blanket confirmation or ceremony to every
delete. Ask for a compact `y`/`n` confirmation only when the freshly measured
target is risky: recent, at least 1 GiB, shared/global, unknown-age, or dependent
on a package download for restoration. Catalog entries marked scan-only must
never be deleted.

## Product principles learned in the 0.2.0 session

1. Directness is part of safety. A clear focused-item action is easier to
   understand than a multi-step selection workflow.
2. Guardrails should be proportional and contextual. Low-risk cache deletion
   should be one keypress; risky deletion should require one explicit answer.
3. Safety checks belong close to mutation, not as repeated user friction.
   Pressing `d` remeasures in the background, and deletion revalidates path,
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
   shared renv cache are scan-only; Docker build data requires a future native
   Docker integration rather than directory deletion.

## Current release state

- Current version: `0.2.0`, released 2026-08-18.
- Published to crates.io: <https://crates.io/crates/cacheferret/0.2.0>
- Published to PyPI: <https://pypi.org/project/cacheferret/0.2.0/>
- PyPI provides native x86_64/aarch64 wheels for macOS and manylinux2014 Linux,
  plus an sdist. Windows is not currently a supported wheel target.
- The release was verified with fresh public installs from both registries;
  both binaries reported `cacheferret 0.2.0`.
- The local annotated `v0.2.0` tag points to release commit `f273b99`.
- A public GitHub repository was created at `rvben/cacheferret`, but source and
  tag publication were not completed during the session. Check remote state
  before assuming GitHub releases, badges, or compare links work.
- Never copy local Cargo or PyPI credentials into GitHub secrets without the
  user's explicit authorization. Registry publishing can be performed locally;
  release workflows skip their registry/Homebrew steps when the corresponding
  repository secret is absent.

## Release and quality baseline

Before release, run the documented checklist in `docs/releasing.md`. The 0.2.0
baseline passed:

- `make check`: formatting, Clippy, and 34 tests.
- `make conformance`: 24/24 clispec.dev checks.
- `cargo audit`: no known advisories.
- `cargo publish --dry-run --locked` and Cargo package verification.
- strict Twine checks for all five Python artifacts.
- clean installs from public PyPI and crates.io.

Python packaging uses Maturin binary bindings; it distributes the same Rust
executable rather than a Python reimplementation. Keep Cargo and Python package
versions synchronized.

