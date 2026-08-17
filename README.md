<p align="center">
  <img src="assets/cacheferret-logo-concept.png" alt="CacheFerret" width="720">
</p>

# CacheFerret

[![CI](https://github.com/rvben/cacheferret/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/cacheferret/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/cacheferret.svg)](https://crates.io/crates/cacheferret)
[![clispec](https://img.shields.io/badge/clispec-v0.3-3b82f6)](https://clispec.dev)

CacheFerret finds rebuildable developer caches across macOS and Linux, shows
where the disk space went, and removes only explicitly selected, revalidated
targets.

Running `cacheferret` without a command is always read-only.

## Install

```sh
cargo install cacheferret
```

Release archives and a Homebrew formula are also produced for Intel and ARM
Linux and macOS.

## Quick start

```sh
# Scan common source roots plus shared caches
cacheferret

# Scan one source tree
cacheferret scan --root ~/Projects --scope project

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

Output is human-readable on a terminal and JSON when piped:

```sh
cacheferret scan --limit 20 --fields kind,path,bytes |
  jq '.items[] | select(.bytes > 1073741824)'
```

## Safety model

- Bare `cacheferret` and `cacheferret scan` never mutate the filesystem.
- `clean` defaults to project caches; shared global caches require an explicit
  `--scope global` or `--scope all`.
- Caches modified in the last seven days are protected unless
  `--include-recent` is passed. Change the window with `--protect-days`.
- A non-interactive clean refuses to run without `--yes` and exits with the
  declared `confirmation_required` error.
- Cache roots must match a closed catalog and their project ownership markers.
- Symlinks are never followed.
- Immediately before each deletion, CacheFerret checks the path, filesystem
  identity, scan-root containment, kind, and ownership markers again.
- Targets that need package downloads to restore are identified in scan and
  clean output.
- Shared stores that can contain or back irreplaceable project state are
  scan-only and remain excluded even with `--include-recent --yes`.
- `--dry-run` follows the same discovery and eligibility policy without
  deleting anything, and lists every selected path with its size and restore
  requirements.

Reclaimed bytes are an estimate based on the freshly measured directory size;
filesystem free-space deltas can differ because of snapshots, compression,
hard links, or concurrent writes.

## Supported caches

`cacheferret catalog` returns the complete machine-readable catalog. The first
release covers:

| ecosystem | project caches | shared caches |
| --- | --- | --- |
| Rust | Cargo `target/` | Cargo registry and git checkouts |
| Python | virtualenvs, bytecode, pytest, mypy, Ruff, tox, nox | pip and uv |
| JavaScript | `node_modules` | npm, pnpm, Bun, Deno |
| Go | — | compiler and module caches |
| JVM/Android | Gradle output and project cache, Maven `target/` | Gradle; Maven repository (scan-only) |
| .NET | `bin/`, `obj/` | NuGet packages |
| Ruby/PHP | Bundler and Composer dependencies | RubyGems and Composer caches |
| Swift | SwiftPM `.build/` | SwiftPM caches and Xcode DerivedData |
| C/C++ | verified CMake build trees | ccache |
| Zig/Dart/Elixir | project build and dependency state | Zig, pub, and Hex caches |
| Haskell | Stack and Cabal project output | Stack and Cabal stores |
| Terraform/R | modules, providers, renv project libraries | configured provider; renv cache (scan-only) |
| Other | any directory with a valid `CACHEDIR.TAG` | — |

Docker build data is intentionally not treated as a directory cache. It needs a
separate native `docker builder prune` integration with Docker-aware sizing and
is planned as a follow-up.

The Maven local repository is scan-only because it may contain unpublished
locally installed artifacts. The shared renv cache is scan-only because project
libraries may link packages from it.

## Commands

| command | behavior |
| --- | --- |
| `cacheferret` | Scan using safe defaults |
| `cacheferret scan` | Scan with root, scope, kind, pagination, and field controls |
| `cacheferret clean` | Preview or clean eligible caches |
| `cacheferret catalog` | List supported cache kinds and restore costs |
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
- offset pagination and `--fields` for the unbounded scan result;
- honest `read_only` and `idempotent` effect declarations.

```sh
cacheferret schema
cacheferret schema clean
```

| exit | kind | meaning |
| ---: | --- | --- |
| `0` | — | Success, including a no-op clean |
| `2` | `invalid_input` | Invalid root, cache kind, field, or value |
| `3` | `usage` | Invalid command-line invocation |
| `4` | `io` | Filesystem or process operation failed |
| `5` | `conflict` | Every target changed or became unsafe before deletion |
| `6` | `confirmation_required` | A non-TTY clean omitted `--yes` |

## Development

```sh
make check        # format, clippy, and tests
make conformance  # build and score the CLI against clispec.dev
```

The generated mascot and wordmark in `assets/` are initial brand concepts. A
future design pass can trace the chosen mark into deterministic SVG assets.

## License

MIT
