<p align="center">
  <img src="assets/cacheferret-logo-concept.png" alt="CacheFerret" width="720">
</p>

# CacheFerret

[![CI](https://github.com/rvben/cacheferret/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/cacheferret/actions/workflows/ci.yml)
[![public installs](https://github.com/rvben/cacheferret/actions/workflows/install-smoke.yml/badge.svg)](https://github.com/rvben/cacheferret/actions/workflows/install-smoke.yml)
[![crates.io](https://img.shields.io/crates/v/cacheferret.svg)](https://crates.io/crates/cacheferret)
[![clispec](https://img.shields.io/badge/clispec-v0.3-3b82f6)](https://clispec.dev)

CacheFerret finds rebuildable developer caches across macOS and Linux, shows
where the disk space went, and removes the caches you choose. Run it in a
terminal for a fast, keyboard-first workspace; pipe it for structured JSON.

Opening the TUI starts with a scan. Press `Space` to select caches quickly, then
`d` to delete the batch; with no selection, `d` deletes the focused cache.
Recent, large, shared, unknown-age, and download-backed caches ask first.

## Install

```sh
# Homebrew on macOS or Linux
brew install rvben/tap/cacheferret

# Cargo
cargo install cacheferret

# PyPI / pipx
pipx install cacheferret
```

Release archives include checksums, documentation, and completions for Bash,
Zsh, Fish, PowerShell, and Elvish on Intel and ARM Linux and macOS.

## Quick start

```sh
# Open the interactive cache workspace
cacheferret

# Open the workspace for one source tree
cacheferret tui --root ~/Projects --scope project

# Produce plain or structured scan output without opening the TUI
cacheferret scan --root ~/Projects --scope project

# Inspect Docker-managed storage without pruning anything
cacheferret docker

# Preview the bounded Docker build-cache cleanup
cacheferret docker clean --dry-run

# Confirm Docker build-cache cleanup from a script or agent
cacheferret docker clean --yes

# Preview old project caches that are eligible for cleanup
cacheferret clean --root ~/Projects --dry-run

# Clean after an interactive confirmation
cacheferret clean --root ~/Projects

# Confirm from a script or agent
cacheferret clean --root ~/Projects --yes

# Shared caches are never part of the default clean scope
cacheferret clean --scope global --include-recent --dry-run
cacheferret clean --scope global --include-recent --yes
```

Inside the TUI, use the arrow keys or `j`/`k` to move, `Space` to select and
advance, `a` to toggle all visible caches, and `d` to delete the selection or
focused cache. Use `/` to filter and `Tab` to cycle scopes. Risky batches use a
single compact `y`/`n` prompt. Press `?` for the complete shortcut guide.
Catalog entries marked scan-only are excluded from focused deletion, `a` batch
selection, and CLI cleanup. Select one individually with `Space` to request a
manual override; `d` then requires confirmation and repeats the safety checks.
Docker build cache participates in the same selection model and always asks for
confirmation. Docker images, containers, and volumes remain inspection-only.

After a successful TUI scan, CacheFerret keeps a small private snapshot of known
cache paths. Later launches show those paths immediately with approximate prior
sizes while fresh measurements run in the background; stale rows cannot be
selected or deleted. Set `CACHEFERRET_NO_CACHE=1` to disable this warm-start
accelerator.

CacheFerret adapts automatically to truecolor, 256-color, basic ANSI, no-color, and
non-UTF-8 terminals. Set `NO_COLOR=1` for an uncolored interface,
`CACHEFERRET_ASCII=1` for ASCII-only glyphs, or
`CACHEFERRET_REDUCE_MOTION=1` for static progress indicators.

Output is human-readable on a terminal and JSON when piped:

```sh
cacheferret scan --limit 20 --fields kind,path,bytes |
  jq '.items[] | select(.bytes > 1073741824)'

cacheferret docker --fields kind,reclaimable_bytes |
  jq '.items[] | select(.reclaimable_bytes > 1073741824)'
```

## Safety model

- Bare `cacheferret` opens the TUI on a terminal and emits a read-only JSON scan
  when piped. `cacheferret scan` never mutates the filesystem.
- The TUI supports both focused and batch deletion: press `Space` to build a
  selection, then `d`; when nothing is selected, `d` acts on the focused cache.
- Pressing `d` remeasures every target before deciding whether confirmation is
  required. A risky batch gets one confirmation, and every target receives
  another identity and ownership check immediately before removal.
- `clean` defaults to project caches; shared global caches require an explicit
  `--scope global` or `--scope all`.
- The batch `clean` command protects caches modified in the last seven days
  unless `--include-recent` is passed. Change the window with `--protect-days`.
- A non-interactive clean refuses to run without `--yes` and exits with the
  declared `confirmation_required` error.
- Cache roots must match a closed catalog and their project ownership markers.
- Symlinks are never followed.
- Immediately before each deletion, CacheFerret checks the path, filesystem
  identity, scan-root containment, kind, and ownership markers again.
- Targets that need package downloads to restore are identified in scan and
  clean output.
- Shared stores that can contain or back irreplaceable project state are
  scan-only and remain excluded from CLI cleanup even with
  `--include-recent --yes`. The TUI permits deletion only after individual
  `Space` selection and an explicit override confirmation.
- On macOS, CacheFerret recognizes Chrome code-signing clones and strongly
  identified build caches in system temporary storage. Large temporary project
  workspaces are visible but scan-only because they may contain unique work;
  they require the same individual TUI override.
- Temporary caches must remain unchanged between their final measurement and
  deletion. Active writers cause cleanup to stop with a conflict.
- `--dry-run` follows the same discovery and eligibility policy without
  deleting anything, and lists every selected path with its apparent size,
  allocated-block estimate, and restore requirements.
- Docker storage is inspected through a bounded native command. Only ordinary
  build cache is selectable. Cleanup refreshes the estimate before confirmation
  and again before mutation, then runs exactly `docker builder prune --force`.
  CacheFerret never adds `--all`, never runs `docker system prune`, and never
  prunes images, containers, or volumes.

CacheFerret reports storage in three deliberately separate layers:

- **Apparent bytes** are the logical file lengths, with hard links counted once.
- **Allocated bytes** are the filesystem blocks attributed to the tree before
  deletion. This is still only an upper-bound estimate on APFS because cloned
  files can share those blocks with files outside the deleted tree.
- **Observed disk-free change** is sampled immediately before and after a real
  cleanup. It is the strongest available answer to “what did this cleanup free?”
  but remains a net filesystem measurement, so concurrent writes, snapshots,
  delayed reclamation, compression, and shared clone blocks can make it differ
  from both size estimates—or even make it negative.

JSON exposes the explicit `apparent_bytes_*`, `allocated_bytes_*`, and
`filesystem_deltas` fields. Free-space deltas remain per filesystem and are not
summed across volumes, because APFS volumes may share one underlying storage
pool. The older `bytes_selected` and `bytes_reclaimed_estimate` fields remain as
apparent-byte compatibility aliases.

Docker-managed storage is reported separately using Docker's native total and
reclaimable estimates. These values are not filesystem-path measurements and
are never folded into apparent, allocated, or observed-free-space totals.

## Supported caches

`cacheferret catalog` returns the complete machine-readable catalog. The first
release covers:

| ecosystem | project caches | shared caches |
| --- | --- | --- |
| Rust | Cargo `target/` | Cargo registry and git checkouts |
| Python | virtualenvs, bytecode, pytest, mypy, Ruff, tox, nox | pip, uv, and pre-commit/prek |
| JavaScript | `node_modules` | npm, pnpm, Bun, Deno, and Playwright |
| Go | — | compiler and module caches |
| JVM/Android | Gradle output and project cache, Maven `target/` | Gradle and Plugin Verifier; Maven repository (scan-only) |
| .NET | `bin/`, `obj/` | NuGet packages |
| Ruby/PHP | Bundler and Composer dependencies | RubyGems and Composer caches |
| Swift | SwiftPM `.build/` | SwiftPM caches and Xcode DerivedData |
| C/C++ | verified CMake build trees | ccache |
| Zig/Dart/Elixir | project build and dependency state | Zig, pub, and Hex caches |
| Haskell | Stack and Cabal project output | Stack and Cabal stores |
| Terraform/R | modules, providers, renv project libraries | configured provider; renv cache (scan-only) |
| macOS | — | Chrome signing clones, temporary build caches, and large temporary workspaces (scan-only) |
| Other | any directory with a valid `CACHEDIR.TAG` | — |

Docker build data is intentionally not treated as a directory cache.
`cacheferret docker` uses `docker system df` to report images, containers,
volumes, and build cache as distinct native resources. The TUI shows the same
daemon-scoped rows when global storage is in scope. Ordinary build cache alone
can be selected and pruned; every prune gets a fresh preview and explicit
confirmation. The bounded adapter contract and native cleanup opportunities for
other package managers are documented in
[docs/native-cleanup.md](docs/native-cleanup.md).

The Maven local repository is scan-only because it may contain unpublished
locally installed artifacts. The shared renv cache is scan-only because project
libraries may link packages from it.

Temporary-storage discovery is deliberately narrow. CacheFerret only considers
directories owned by the current user, never follows symlinks, and does not
treat arbitrary `/private/tmp` contents as deletable. Recognizable build/cache
names, exact Go build-cache metadata, and Xcode DerivedData structures are
cleanable. Valid `CACHEDIR.TAG` roots nested inside any owned temporary tree are
measured and protected by their own activity, independently of the parent; they
take precedence in that scan to avoid double-counting their bytes. Large
workspaces without a recognized nested cache are reported as scan-only
diagnostic findings. Global cleanup remains opt-in, and recent cache entries
remain protected unless `--include-recent` is supplied.

## Commands

| command | behavior |
| --- | --- |
| `cacheferret` | Open the TUI on a terminal; scan as JSON when piped |
| `cacheferret tui` | Open the interactive browser with optional discovery filters |
| `cacheferret scan` | Scan with root, scope, kind, pagination, and field controls |
| `cacheferret docker` | Inspect Docker storage with pagination and field controls |
| `cacheferret docker clean` | Preview or confirm bounded ordinary build-cache pruning |
| `cacheferret clean` | Preview or clean eligible caches |
| `cacheferret catalog` | List supported cache kinds with pagination and field controls |
| `cacheferret schema [path]` | Print or narrow the clispec.dev v0.3 contract |
| `cacheferret completions <shell>` | Generate shell completions |

Use `cacheferret <command> --help` for every option.

## Agent contract

CacheFerret follows [clispec.dev v0.3](https://clispec.dev):

- explicit JSON via `--output json` and automatic JSON when piped;
- data on stdout and diagnostics on stderr;
- structured error envelopes as the last stderr line in JSON mode;
- offline `schema` introspection with effects, cardinality, pagination, output
  fields, confirmation gates, and stable exit codes;
- offset pagination and `--fields` for unbounded scan and Docker results;
- honest `read_only` and `idempotent` effect declarations.

```sh
cacheferret schema
cacheferret schema clean
cacheferret schema docker
cacheferret schema docker clean
```

| exit | kind | meaning |
| ---: | --- | --- |
| `0` | — | Success, including a no-op clean |
| `2` | `invalid_input` | Invalid root, cache kind, field, or value |
| `3` | `usage` | Invalid command-line invocation |
| `4` | `io` | Filesystem or process operation failed |
| `5` | `conflict` | Every target changed or became unsafe before deletion |
| `6` | `confirmation_required` | A non-TTY clean omitted `--yes` |
| `7` | `native_unavailable` | A native tool, daemon, or operation is unavailable; retryable |
| `8` | `native_protocol` | Native output could not be interpreted safely |

## Development

```sh
make check        # format, clippy, and tests
make conformance  # build and score the CLI against clispec.dev
```

See [docs/releasing.md](docs/releasing.md) for the release checklist and
[docs/product.md](docs/product.md) for the durable product direction. See
[SECURITY.md](SECURITY.md) for private vulnerability reporting.

The generated mascot and wordmark in `assets/` are initial brand concepts. A
future design pass can trace the chosen mark into deterministic SVG assets.

## License

MIT
